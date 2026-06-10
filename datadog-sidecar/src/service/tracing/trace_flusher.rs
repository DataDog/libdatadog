// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::TraceSendData;
use crate::agent_remote_config::AgentRemoteConfigWriter;
use datadog_ipc::platform::NamedShmHandle;
use futures::future::join_all;
use libdd_capabilities_impl::{HttpClientCapability, NativeCapabilities};
use libdd_common::{Endpoint, MutexExt};
use libdd_trace_utils::trace_utils;
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::trace_utils::SendDataResult;
use manual_future::{ManualFuture, ManualFutureCompleter};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tracing::{error, info};

const DEFAULT_FLUSH_INTERVAL_MS: u64 = 5_000;
const DEFAULT_MIN_FORCE_FLUSH_SIZE_BYTES: u32 = 1_000_000;

/// `TraceFlusherStats` holds stats of the trace flusher like the count of allocated shared memory
/// for agent config, agent config writers, last used entries in agent configs, and the size of send
/// data.
#[derive(Debug, Serialize, Deserialize)]
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

/// Per-endpoint flush state. Each distinct agent endpoint gets its own send buffer and its own
/// flusher task, so a slow or unreachable endpoint (e.g. a dead agent that retries for many
/// seconds) cannot stall delivery to healthy endpoints. In the common production case there is a
/// single endpoint, so this behaves exactly like the previous single-buffer design.
#[derive(Default)]
struct PerEndpoint {
    traces: TraceSendData,
    flusher: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct TraceFlusherData {
    endpoints: HashMap<Endpoint, PerEndpoint>,
}

/// Upper bound on how long a synchronous (global) flush waits for endpoints to drain. The flush is
/// best-effort: a stuck endpoint keeps retrying in its own background task, but it must never block
/// the caller (and thus the blocking IPC flush) past this bound.
const SYNC_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

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
    capabilities: NativeCapabilities,
}
impl Default for TraceFlusher {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TraceFlusherData::default()),
            interval_ms: AtomicU64::new(DEFAULT_FLUSH_INTERVAL_MS),
            min_force_flush_size_bytes: AtomicU32::new(DEFAULT_MIN_FORCE_FLUSH_SIZE_BYTES),
            min_force_drop_size_bytes: AtomicU32::new(trace_utils::MAX_PAYLOAD_SIZE as u32),
            remote_config: Mutex::new(Default::default()),
            metrics: Mutex::new(Default::default()),
            capabilities: NativeCapabilities::new_client(),
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
        if data.len() > self.min_force_drop_size_bytes.load(Ordering::Relaxed) as usize {
            error!(
                "Error sending trace. Individual trace size of {}B exceeds {}B limit",
                data.len(),
                self.min_force_drop_size_bytes.load(Ordering::Relaxed) as usize
            );
            return;
        }

        let endpoint = data.get_target().clone();
        let mut flush_data = self.inner.lock_or_panic();
        let per = flush_data.endpoints.entry(endpoint.clone()).or_default();

        per.traces.send_data_size += data.len();
        per.traces.send_data.push(data);

        if per.flusher.is_none() {
            let (force_flush, completer) = ManualFuture::new();
            // The flusher task is scoped to this endpoint and self-terminates (removing this map
            // entry) once its buffer drains, so endpoints don't accumulate idle tasks.
            per.flusher = Some(self.clone().start_trace_flusher(endpoint, force_flush));
            per.traces.force_flush = Some(completer);
        }

        if per.traces.send_data_size
            > self.min_force_flush_size_bytes.load(Ordering::Relaxed) as usize
        {
            per.traces.flush();
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
        let flushers: Vec<JoinHandle<()>> = {
            let mut flush_data = self.inner.lock_or_panic();
            self.interval_ms.store(0, Ordering::SeqCst);
            flush_data
                .endpoints
                .values_mut()
                .filter_map(|per| {
                    per.traces.flush();
                    per.flusher.take()
                })
                .collect()
        };
        for flusher in flushers {
            flusher.await?;
        }
        Ok(())
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
        let rc = self.remote_config.lock_or_panic();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self
                .inner
                .lock_or_panic()
                .endpoints
                .values()
                .map(|per| per.traces.send_data_size as u32)
                .sum(),
        }
    }

    pub fn collect_metrics(&self) -> TraceFlusherMetrics {
        std::mem::take(&mut self.metrics.lock_or_panic())
    }

    fn write_remote_configs(&self, endpoint: Endpoint, contents: Vec<u8>) {
        let configs = &mut *self.remote_config.lock_or_panic();

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
            #[allow(clippy::unwrap_used)]
            configs
                .writers
                .remove(&configs.last_used.remove(&time).unwrap());
        }
    }

    fn replace_trace_send_data(
        &self,
        endpoint: &Endpoint,
        completer: ManualFutureCompleter<Option<mpsc::Sender<()>>>,
    ) -> Vec<SendData> {
        let mut flush_data = self.inner.lock_or_panic();
        let trace_buffer = match flush_data.endpoints.get_mut(endpoint) {
            Some(per) => {
                std::mem::replace(
                    &mut per.traces,
                    TraceSendData {
                        send_data: vec![],
                        send_data_size: 0,
                        force_flush: Some(completer),
                    },
                )
                .send_data
            }
            None => vec![],
        };
        trace_utils::coalesce_send_data(trace_buffer)
            .into_iter()
            .collect()
    }

    async fn send_and_handle_trace(&self, send_data: SendData) {
        let endpoint = send_data.get_target().clone();
        let response = send_data.send(&self.capabilities).await;
        self.metrics.lock_or_panic().update(&response);
        match response.last_result {
            Ok(response) => {
                if endpoint.api_key.is_none() {
                    // not when intake
                    self.write_remote_configs(endpoint.clone(), response.into_body().to_vec());
                }
                info!("Successfully flushed traces to {endpoint:?}");
            }
            Err(e) => {
                error!("Error sending trace: {e:?}");
            }
        }
    }

    fn start_trace_flusher(
        self: Arc<Self>,
        endpoint: Endpoint,
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

                let (new_force_flush, completer) = ManualFuture::new();
                force_flush = new_force_flush;

                // Swap out (and coalesce) only this endpoint's buffer, then send without holding
                // the lock. Other endpoints' flushers run independently and concurrently.
                let send_data = self.replace_trace_send_data(&endpoint, completer);
                join_all(send_data.into_iter().map(|d| self.send_and_handle_trace(d))).await;

                drop(flush_done_sender);

                // Reap this endpoint's task once its buffer has drained, so dead/idle endpoints
                // don't accumulate tasks or map entries. enqueue() will spawn a fresh task (and
                // re-create the entry) if more data arrives for this endpoint later.
                let mut data = self.inner.lock_or_panic();
                match data.endpoints.get(&endpoint) {
                    Some(per) if per.traces.send_data.is_empty() => {
                        data.endpoints.remove(&endpoint);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        })
    }

    /// Flushes immediately without delay. Triggers a flush on every endpoint and waits, bounded by
    /// `SYNC_FLUSH_TIMEOUT`, for them to drain. This is best-effort: a slow or unreachable endpoint
    /// keeps retrying in its own background task and must not block the caller (the blocking IPC
    /// flush) past the bound.
    pub async fn flush(&self) {
        let flush_dones: Vec<_> = {
            let mut flush_data = self.inner.lock_or_panic();
            flush_data
                .endpoints
                .values_mut()
                .map(|per| per.traces.await_flush())
                .collect()
        };
        if flush_dones.is_empty() {
            return;
        }
        let _ = tokio::time::timeout(SYNC_FLUSH_TIMEOUT, join_all(flush_dones)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use libdd_trace_utils::test_utils::{create_send_data, poll_for_mock_hit};
    use std::sync::Arc;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // Test scenario: Enqueue two traces with a size less than the minimum force flush size, and
    // observe that a request to the trace agent is not made. Then enqueue a third trace exceeding
    // the min force flush size, and observe that a request to the trace agent is made.
    async fn test_min_flush_size() {
        // Set the interval high enough that it can't cause a false positive
        let trace_flusher = Arc::new(TraceFlusher {
            interval_ms: AtomicU64::new(20_000),
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
            .min_force_flush_size_bytes
            .load(Ordering::Relaxed) as usize
            / 2;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
            ..Default::default()
        };

        let send_data_1 = create_send_data(size, &target_endpoint);
        let send_data_2 = create_send_data(size, &target_endpoint);
        let send_data_3 = create_send_data(size, &target_endpoint);

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
    // Test scenario: Enqueue a trace with a size greater than the minimum force drop size, and
    // observe that it is not sent.
    async fn test_drop_size_no_flush() {
        // Set the interval high enough that it can't cause a false positive
        let trace_flusher = Arc::new(TraceFlusher {
            interval_ms: AtomicU64::new(20_000),
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
