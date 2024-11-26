// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::TraceSendData;
use crate::agent_remote_config::AgentRemoteConfigWriter;
use datadog_ipc::platform::NamedShmHandle;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::SendDataResult;
use ddcommon_net1::Endpoint;
use futures::future::join_all;
use hyper::body::HttpBody;
use manual_future::{ManualFuture, ManualFutureCompleter};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::ops::DerefMut;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::sync::mpsc;
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
    pub bytes_sent: u64,
    pub chunks_sent: u64,
    pub chunks_dropped: u64,
}

impl TraceFlusherMetrics {
    fn update(&mut self, result: &SendDataResult) {
        self.api_requests += result.requests_count;
        self.api_errors_timeout += result.errors_timeout;
        self.api_errors_network += result.errors_network;
        self.api_errors_status_code += result.errors_status_code;
        self.bytes_sent += result.bytes_sent;
        self.chunks_sent += result.chunks_sent;
        self.chunks_dropped += result.chunks_dropped;

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

        flush_data.traces.send_data_size += data.len();

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
    ///
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

    fn replace_trace_send_data(
        &self,
        completer: ManualFutureCompleter<Option<mpsc::Sender<()>>>,
    ) -> Vec<SendData> {
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

    async fn send_and_handle_trace(&self, send_data: SendData) {
        let endpoint = send_data.get_target().clone();
        let response = send_data.send().await;
        self.metrics.lock().unwrap().update(&response);
        match response.last_result {
            Ok(response) => {
                if endpoint.api_key.is_none() {
                    // not when intake
                    match response.into_body().collect().await {
                        Ok(body) => {
                            self.write_remote_configs(endpoint.clone(), body.to_bytes().to_vec())
                        }
                        Err(e) => error!("Error receiving agent configuration: {e:?}"),
                    }
                }
                info!("Successfully flushed traces to {}", endpoint.url);
            }
            Err(e) => {
                error!("Error sending trace: {e:?}");
            }
        }
    }

    fn start_trace_flusher(
        self: Arc<Self>,
        mut force_flush: ManualFuture<Option<mpsc::Sender<()>>>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut flush_done_sender = None;
                select! {
                    _ = tokio::time::sleep(Duration::from_millis(
                        self.interval_ms.load(Ordering::Relaxed),
                    )) => {},
                    sender = force_flush => { flush_done_sender = sender; },
                }

                debug!(
                    "Start flushing {} bytes worth of traces",
                    self.inner.lock().unwrap().traces.send_data_size
                );

                let (new_force_flush, completer) = ManualFuture::new();
                force_flush = new_force_flush;

                let send_data = self.replace_trace_send_data(completer);
                join_all(send_data.into_iter().map(|d| self.send_and_handle_trace(d))).await;

                drop(flush_done_sender);

                let mut data = self.inner.lock().unwrap();
                let data = data.deref_mut();
                if data.traces.send_data.is_empty() {
                    data.flusher = None;
                    break;
                }
            }
        })
    }

    /// Flushes immediately without delay.
    pub async fn flush(&self) {
        let flush_done = self.inner.lock().unwrap().traces.await_flush();
        flush_done.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_utils::test_utils::{create_send_data, poll_for_mock_hit};
    use ddcommon_net1::Endpoint;
    use httpmock::MockServer;
    use std::sync::Arc;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // Test scenario: Enqueue two traces with a size less than the minimum force flush size, and
    // observe that a request to the trace agent is not made. Then enqueue a third trace exceeding
    // the min force flush size, and observe that a request to the trace agent is made.
    async fn test_min_flush_size() {
        let trace_flusher = Arc::new(TraceFlusher::default());

        let server = MockServer::start();

        let mut mock = server
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
            ..Default::default()
        };

        let send_data_1 = create_send_data(size, &target_endpoint);

        let send_data_2 = send_data_1.clone();
        let send_data_3 = send_data_1.clone();

        trace_flusher.enqueue(send_data_1);
        trace_flusher.enqueue(send_data_2);

        assert!(poll_for_mock_hit(&mut mock, 10, 150, 0, false).await);

        // enqueue a trace that exceeds the min force flush size
        trace_flusher.enqueue(send_data_3);

        assert!(poll_for_mock_hit(&mut mock, 25, 100, 1, true).await);
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
        let mut mock = server
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
            ..Default::default()
        };
        let send_data_1 = create_send_data(size, &target_endpoint);

        trace_flusher.enqueue(send_data_1);

        // Sleep for a duration longer than the flush interval
        tokio::time::sleep(Duration::from_millis(
            trace_flusher.interval_ms.load(Ordering::Relaxed) + 1,
        ))
        .await;
        assert!(poll_for_mock_hit(&mut mock, 25, 100, 1, true).await);
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
        let mut mock = server
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
            ..Default::default()
        };

        let send_data_1 = create_send_data(size, &target_endpoint);

        trace_flusher.enqueue(send_data_1);

        assert!(poll_for_mock_hit(&mut mock, 5, 250, 0, true).await);
    }
}
