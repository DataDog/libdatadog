// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::TraceSendData;
use crate::agent_remote_config::AgentRemoteConfigWriter;
use datadog_ipc::platform::NamedShmHandle;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::SendDataResult;
use ddcommon::Endpoint;
use futures::future::join_all;
use manual_future::{ManualFuture, ManualFutureCompleter};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::iter::zip;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, info};

const DEFAULT_FLUSH_INTERVAL_MS: u64 = 5_000;
const DEFAULT_MIN_FORCE_FLUSH_SIZE_BYTES: u32 = 1_000_000;
const DEFAULT_MIN_FORCE_DROP_SIZE_BYTES: u32 = 10_000_000;

/// `TraceFlusherStats` holds stats of the trace flusher like the count of allocated shared memory
/// for agent config, agent config writers, last used entries in agent configs, and the size of send
/// data.
#[derive(Serialize, Deserialize)]
pub(crate) struct TraceFlusherStats {
    pub(crate) agent_config_allocated_shm: u32,
    pub(crate) agent_config_writers: u32,
    pub(crate) agent_configs_last_used_entries: u32,
    pub(crate) send_data_size: u32,
}

struct AgentRemoteConfig {
    writer: AgentRemoteConfigWriter<NamedShmHandle>,
    last_write: Instant,
}

#[derive(Default)]
struct AgentRemoteConfigs {
    writers: HashMap<Endpoint, AgentRemoteConfig>,
    last_used: BTreeMap<Instant, Endpoint>,
}

#[derive(Default)]
struct TraceFlusherData {
    traces: TraceSendData,
    flusher: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct TraceFlusherMetrics {
    pub api_requests: u64,
    pub api_responses_count_per_code: HashMap<u16, u64>,
    pub api_errors_timeout: u64,
    pub api_errors_network: u64,
    pub api_errors_status_code: u64,
}

impl TraceFlusherMetrics {
    fn update(&mut self, result: &SendDataResult) {
        self.api_requests += result.requests_count;
        self.api_errors_timeout += result.errors_timeout;
        self.api_errors_network += result.errors_network;
        self.api_errors_status_code += result.errors_status_code;

        for (status_code, count) in &result.responses_count_per_code {
            *self
                .api_responses_count_per_code
                .entry(*status_code)
                .or_default() += count;
        }
    }
}

/// `TraceFlusher` is a structure that manages the flushing of traces.
/// It contains the traces to be sent, the flusher task, the interval for flushing,
/// the minimum sizes for force flushing and dropping, and the remote configs.
pub(crate) struct TraceFlusher {
    inner: Mutex<TraceFlusherData>,
    pub(crate) interval_ms: AtomicU64,
    pub(crate) min_force_flush_size_bytes: AtomicU32,
    pub(crate) min_force_drop_size_bytes: AtomicU32, // put a limit on memory usage
    remote_config: Mutex<AgentRemoteConfigs>,
    pub metrics: Mutex<TraceFlusherMetrics>,
}
impl Default for TraceFlusher {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TraceFlusherData::default()),
            interval_ms: AtomicU64::new(DEFAULT_FLUSH_INTERVAL_MS),
            min_force_flush_size_bytes: AtomicU32::new(DEFAULT_MIN_FORCE_FLUSH_SIZE_BYTES),
            min_force_drop_size_bytes: AtomicU32::new(DEFAULT_MIN_FORCE_DROP_SIZE_BYTES),
            remote_config: Mutex::new(Default::default()),
            metrics: Mutex::new(Default::default()),
        }
    }
}
impl TraceFlusher {
    /// Enqueue a `SendData` to the traces and triggers a flush if the size exceeds the minimum
    /// force flush size.
    ///
    /// # Arguments
    ///
    /// * `data` - A `SendData` instance that needs to be added to the traces.
    pub(crate) fn enqueue(self: &Arc<Self>, data: SendData) {
        let mut flush_data = self.inner.lock().unwrap();
        let flush_data = flush_data.deref_mut();

        flush_data.traces.send_data_size += data.size;

        if flush_data.traces.send_data_size
            > self.min_force_drop_size_bytes.load(Ordering::Relaxed) as usize
        {
            return;
        }

        flush_data.traces.send_data.push(data);
        if flush_data.flusher.is_none() {
            let (force_flush, completer) = ManualFuture::new();
            flush_data.flusher = Some(self.clone().start_trace_flusher(force_flush));
            flush_data.traces.force_flush = Some(completer);
        }
        if flush_data.traces.send_data_size
            > self.min_force_flush_size_bytes.load(Ordering::Relaxed) as usize
        {
            flush_data.traces.flush();
        }
    }

    /// Join the flusher task and flush the remaining traces.
    ///
    /// # Returns
    ///
    /// * A `Result` which is `Ok` if the flusher task successfully joins, or `Err` if the flusher
    ///   task panics.
    /// If the flusher task is not running, it returns `Ok`.
    pub(crate) async fn join(&self) -> anyhow::Result<(), JoinError> {
        let flusher = {
            let mut flush_data = self.inner.lock().unwrap();
            self.interval_ms.store(0, Ordering::SeqCst);
            flush_data.traces.flush();
            flush_data.deref_mut().flusher.take()
        };
        if let Some(flusher) = flusher {
            flusher.await
        } else {
            Ok(())
        }
    }

    /// Get the statistics of the trace flusher.
    ///
    /// # Returns
    ///
    /// * A `TraceFlusherStats` instance that contains the statistics of the trace flusher.
    ///
    /// This method retrieves the statistics of the trace flusher, including the count of allocated
    /// shared memory for agent config, agent config writers, last used entries in agent
    /// configs, and the size of send data.
    pub(crate) fn stats(&self) -> TraceFlusherStats {
        let rc = self.remote_config.lock().unwrap();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self.inner.lock().unwrap().traces.send_data_size as u32,
        }
    }

    pub fn collect_metrics(&self) -> TraceFlusherMetrics {
        std::mem::take(&mut self.metrics.lock().unwrap())
    }

    fn write_remote_configs(&self, endpoint: Endpoint, contents: Vec<u8>) {
        let configs = &mut *self.remote_config.lock().unwrap();

        let mut entry = configs.writers.entry(endpoint.clone());
        let writer = match entry {
            Entry::Occupied(ref mut entry) => entry.get_mut(),
            Entry::Vacant(entry) => {
                if let Ok(writer) = crate::agent_remote_config::new_writer(&endpoint) {
                    entry.insert(AgentRemoteConfig {
                        writer,
                        last_write: Instant::now(),
                    })
                } else {
                    return;
                }
            }
        };
        writer.writer.write(contents.as_slice());

        let now = Instant::now();
        let last = writer.last_write;
        writer.last_write = now;

        configs.last_used.remove(&last);
        configs.last_used.insert(now, endpoint);

        while let Some((&time, _)) = configs.last_used.iter().next() {
            if time + Duration::new(50, 0) > Instant::now() {
                break;
            }
            configs
                .writers
                .remove(&configs.last_used.remove(&time).unwrap());
        }
    }

    fn replace_trace_send_data(&self, completer: ManualFutureCompleter<()>) -> Vec<SendData> {
        let trace_buffer = std::mem::replace(
            &mut self.inner.lock().unwrap().traces,
            TraceSendData {
                send_data: vec![],
                send_data_size: 0,
                force_flush: Some(completer),
            },
        )
        .send_data;
        trace_utils::coalesce_send_data(trace_buffer)
            .into_iter()
            .collect()
    }

    async fn send_traces(&self, send_data: Vec<SendData>) {
        let mut futures: Vec<_> = Vec::new();
        let mut intake_target: Vec<_> = Vec::new();
        for send_data in send_data {
            intake_target.push(send_data.target.clone());
            futures.push(send_data.send());
        }
        for (endpoint, response) in zip(intake_target, join_all(futures).await) {
            self.handle_trace_response(endpoint, response).await;
        }
    }

    async fn handle_trace_response(&self, endpoint: Endpoint, response: SendDataResult) {
        self.metrics.lock().unwrap().update(&response);
        match response.last_result {
            Ok(response) => {
                if endpoint.api_key.is_none() {
                    // not when intake
                    match hyper::body::to_bytes(response.into_body()).await {
                        Ok(body_bytes) => {
                            self.write_remote_configs(endpoint.clone(), body_bytes.to_vec())
                        }
                        Err(e) => error!("Error receiving agent configuration: {e:?}"),
                    }
                }
                info!("Successfully flushed traces to {}", endpoint.url);
            }
            Err(e) => {
                error!("Error sending trace: {e:?}");
                if endpoint.api_key.is_some() {
                    // TODO: APMSP-1020 Retries when sending to intake
                }
            }
        }
    }

    fn start_trace_flusher(self: Arc<Self>, mut force_flush: ManualFuture<()>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                select! {
                    _ = tokio::time::sleep(Duration::from_millis(
                        self.interval_ms.load(Ordering::Relaxed),
                    )) => {},
                    _ = force_flush => {},
                }

                debug!(
                    "Start flushing {} bytes worth of traces",
                    self.inner.lock().unwrap().traces.send_data_size
                );

                let (new_force_flush, completer) = ManualFuture::new();
                force_flush = new_force_flush;

                let send_data = self.replace_trace_send_data(completer);
                self.send_traces(send_data).await;

                let mut data = self.inner.lock().unwrap();
                let data = data.deref_mut();
                if data.traces.send_data.is_empty() {
                    data.flusher = None;
                    break;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use httpmock::{Mock, MockServer};
    use std::sync::Arc;

    // This function will poll the mock server for "hits" until the expected number of hits is
    // observed. In its current form it may not correctly report if more than the asserted number of
    // hits occurred. More attempts at lower sleep intervals is preferred to reduce flakiness and
    // test runtime.
    async fn poll_for_mock_hit(
        mock: &Mock<'_>,
        poll_attempts: i32,
        sleep_interval_ms: u64,
        expected_hits: usize,
    ) -> bool {
        let mut mock_hit = mock.hits_async().await == expected_hits;

        let mut mock_observations_remaining = poll_attempts;

        while !mock_hit {
            tokio::time::sleep(Duration::from_millis(sleep_interval_ms)).await;
            mock_hit = mock.hits_async().await == expected_hits;
            mock_observations_remaining -= 1;
            if mock_observations_remaining == 0 || mock_hit {
                break;
            }
        }

        mock_hit
    }

    fn create_send_data(size: usize, target_endpoint: &Endpoint) -> SendData {
        let tracer_header_tags = TracerHeaderTags::default();

        let tracer_payload = pb::TracerPayload {
            container_id: "container_id_1".to_owned(),
            language_name: "php".to_owned(),
            language_version: "4.0".to_owned(),
            tracer_version: "1.1".to_owned(),
            runtime_id: "runtime_1".to_owned(),
            chunks: vec![],
            tags: Default::default(),
            env: "test".to_owned(),
            hostname: "test_host".to_owned(),
            app_version: "2.0".to_owned(),
        };

        SendData::new(size, tracer_payload, tracer_header_tags, target_endpoint)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // Test scenario: Enqueue two traces with a size less than the minimum force flush size, and
    // observe that a request to the trace agent is not made. Then enqueue a third trace exceeding
    // the min force flush size, and observe that a request to the trace agent is made.
    async fn test_min_flush_size() {
        let trace_flusher = Arc::new(TraceFlusher::default());

        let server = MockServer::start();

        let mock = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#);
            })
            .await;

        let size = trace_flusher
            .min_force_flush_size_bytes
            .load(Ordering::Relaxed) as usize
            / 2;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
        };

        let send_data_1 = create_send_data(size, &target_endpoint);

        let send_data_2 = send_data_1.clone();
        let send_data_3 = send_data_1.clone();

        trace_flusher.enqueue(send_data_1);
        trace_flusher.enqueue(send_data_2);

        assert!(poll_for_mock_hit(&mock, 10, 150, 0).await);

        // enqueue a trace that exceeds the min force flush size
        trace_flusher.enqueue(send_data_3);

        assert!(poll_for_mock_hit(&mock, 5, 250, 1).await);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_flush_on_interval() {
        // Set the interval lower than the default to reduce test time
        let trace_flusher = Arc::new(TraceFlusher {
            interval_ms: AtomicU64::new(250),
            ..TraceFlusher::default()
        });
        let server = MockServer::start();
        let mock = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#);
            })
            .await;
        let size = trace_flusher
            .min_force_drop_size_bytes
            .load(Ordering::Relaxed) as usize
            - 1;
        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
        };
        let send_data_1 = create_send_data(size, &target_endpoint);

        trace_flusher.enqueue(send_data_1);

        // Sleep for a duration longer than the flush interval
        tokio::time::sleep(Duration::from_millis(
            trace_flusher.interval_ms.load(Ordering::Relaxed) + 1,
        ))
        .await;
        assert!(poll_for_mock_hit(&mock, 25, 100, 1).await);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_flush_drop_size() {
        // Set the interval high enough that it can't cause a false positive
        let trace_flusher = Arc::new(TraceFlusher {
            interval_ms: AtomicU64::new(10_000),
            ..TraceFlusher::default()
        });
        let server = MockServer::start();
        let mock = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#);
            })
            .await;
        let size = trace_flusher
            .min_force_drop_size_bytes
            .load(Ordering::Relaxed) as usize
            + 1;
        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
        };

        let send_data_1 = create_send_data(size, &target_endpoint);

        trace_flusher.enqueue(send_data_1);

        assert!(poll_for_mock_hit(&mock, 5, 250, 0).await);
    }
}
