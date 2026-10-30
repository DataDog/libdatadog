// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::TraceSendData;
use crate::agent_remote_config::AgentRemoteConfigWriter;
use futures::future::join_all;
use libdd_capabilities_impl::{HttpClientCapability, NativeCapabilities};
use libdd_common::{Endpoint, MutexExt};
use libdd_ipc::platform::NamedShmHandle;
use libdd_trace_utils::trace_utils;
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::trace_utils::SendDataResult;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::ops::DerefMut;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, info};

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
        let mut flush_data = self.inner.lock_or_panic();
        let flush_data = flush_data.deref_mut();

        if data.len() > self.min_force_drop_size_bytes.load(Ordering::Relaxed) as usize {
            error!(
                "Error sending trace. Individual trace size of {}B exceeds {}B limit",
                data.len(),
                self.min_force_drop_size_bytes.load(Ordering::Relaxed) as usize
            );
            return;
        }

        flush_data.traces.send_data_size += data.len();
        flush_data.traces.send_data.push(data);

        if flush_data.flusher.is_none() {
            let (force_flush_tx, force_flush_rx) = oneshot::channel();
            flush_data.flusher = Some(self.clone().start_trace_flusher(force_flush_rx));
            flush_data.traces.force_flush = Some(force_flush_tx);
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
            let mut flush_data = self.inner.lock_or_panic();
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
        let rc = self.remote_config.lock_or_panic();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self.inner.lock_or_panic().traces.send_data_size as u32,
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
        force_flush_tx: oneshot::Sender<Option<mpsc::Sender<()>>>,
    ) -> Vec<SendData> {
        let trace_buffer = std::mem::replace(
            &mut self.inner.lock_or_panic().traces,
            TraceSendData {
                send_data: vec![],
                send_data_size: 0,
                force_flush: Some(force_flush_tx),
            },
        )
        .send_data;
        trace_utils::coalesce_send_data(trace_buffer)
            .into_iter()
            .collect()
    }

    async fn send_and_handle_trace(&self, send_data: SendData) {
        let endpoint = send_data.get_target().clone();
        let response = send_data.send(&self.capabilities).await;
        self.handle_trace_response(endpoint, response);
    }

    fn handle_trace_response(&self, endpoint: Endpoint, response: SendDataResult) {
        self.metrics.lock_or_panic().update(&response);
        if response.errors_timeout != 0
            || response.errors_network != 0
            || response.errors_status_code != 0
            || response.chunks_dropped != 0
        {
            error!(
                errors_timeout = response.errors_timeout,
                errors_network = response.errors_network,
                errors_status_code = response.errors_status_code,
                chunks_dropped = response.chunks_dropped,
                "Error sending trace: one or more requests failed"
            );
            return;
        }
        match response.last_result {
            Ok(response) if response.status().is_success() => {
                if endpoint.api_key.is_none() {
                    // not when intake
                    self.write_remote_configs(endpoint.clone(), response.into_body().to_vec());
                }
                info!("Successfully flushed traces to {endpoint:?}");
            }
            Ok(response) => {
                error!(status = %response.status(), "Error sending trace");
            }
            Err(e) => {
                error!("Error sending trace: {e:?}");
            }
        }
    }

    fn start_trace_flusher(
        self: Arc<Self>,
        mut force_flush_rx: oneshot::Receiver<Option<mpsc::Sender<()>>>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut flush_done_sender = None;
                select! {
                    _ = tokio::time::sleep(Duration::from_millis(
                        self.interval_ms.load(Ordering::Relaxed),
                    )) => {},
                    result = &mut force_flush_rx => {
                        if let Ok(sender) = result {
                            flush_done_sender = sender;
                        }
                    },
                }

                debug!(
                    "Start flushing {} bytes worth of traces",
                    self.inner.lock_or_panic().traces.send_data_size
                );

                let (new_force_flush_tx, new_force_flush_rx) = oneshot::channel();
                force_flush_rx = new_force_flush_rx;

                let send_data = self.replace_trace_send_data(new_force_flush_tx);
                join_all(send_data.into_iter().map(|d| self.send_and_handle_trace(d))).await;

                drop(flush_done_sender);

                let mut data = self.inner.lock_or_panic();
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
        let flush_done = self.inner.lock_or_panic().traces.await_flush();
        flush_done.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use libdd_capabilities::{Bytes, HttpError, SleepCapability};
    use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
    use libdd_trace_utils::test_utils::{create_send_data, poll_for_mock_hit};
    use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
    use std::collections::VecDeque;
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Arc;

    async fn capture_logs(future: impl std::future::Future<Output = ()>) -> String {
        let mut log = tempfile::tempfile().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(log.try_clone().unwrap())
            .with_ansi(false)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        future.await;
        drop(guard);

        let mut text = String::new();
        log.seek(SeekFrom::Start(0)).unwrap();
        log.read_to_string(&mut text).unwrap();
        text
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn http_errors_are_not_logged_or_published_as_success() {
        for intake in [false, true] {
            for (status, attempts) in [(200, 1), (400, 3), (503, 3)] {
                let server = MockServer::start_async().await;
                let response = server.mock(|when, then| {
                    when.method(httpmock::Method::POST).path("/traces");
                    then.status(status).body(r#"{"rate_by_service":{}}"#);
                });
                let endpoint = Endpoint {
                    url: server.url("/traces").parse().unwrap(),
                    api_key: intake.then(|| "test-key".into()),
                    ..Default::default()
                };
                let flusher = TraceFlusher::default();
                let mut data = create_send_data(1, &endpoint);
                data.set_retry_strategy(RetryStrategy::new(2, 1, RetryBackoffType::Constant, None));
                let text = capture_logs(flusher.send_and_handle_trace(data)).await;

                response.assert_calls(attempts);
                let success = status < 300;
                assert_eq!(
                    flusher.stats().agent_config_writers,
                    u32::from(success && !intake),
                    "status {status}"
                );
                let metrics = flusher.collect_metrics();
                assert_eq!(metrics.api_requests, u64::try_from(attempts).unwrap());
                assert_eq!(metrics.api_errors_status_code, u64::from(!success));
                assert_eq!(
                    text.contains("Successfully flushed traces"),
                    success,
                    "{text}"
                );
                assert_eq!(text.contains("Error sending trace"), !success, "{text}");
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Reply {
        Status(u16),
        Network,
        Timeout,
        Body,
        Build,
    }

    #[derive(Clone, Debug, Default)]
    struct ScriptedClient(Arc<Mutex<VecDeque<Reply>>>);

    impl HttpClientCapability for ScriptedClient {
        fn new_client() -> Self {
            Self::default()
        }

        fn new_periodic() -> Self {
            Self::default()
        }

        async fn request(
            &self,
            _: http::Request<Bytes>,
        ) -> Result<http::Response<Bytes>, HttpError> {
            match self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
            {
                Reply::Status(status) => Ok(http::Response::builder()
                    .status(status)
                    .body(Bytes::from_static(br#"{"rate_by_service":{}}"#))
                    .unwrap()),
                Reply::Network => Err(HttpError::Network(anyhow::anyhow!("network failure"))),
                Reply::Timeout => Err(HttpError::Timeout),
                Reply::Body => Err(HttpError::ResponseBody(anyhow::anyhow!("body failure"))),
                Reply::Build => Err(HttpError::InvalidRequest(anyhow::anyhow!(
                    "invalid request"
                ))),
            }
        }
    }

    impl SleepCapability for ScriptedClient {
        fn new() -> Self {
            Self::default()
        }

        async fn sleep(&self, _: Duration) {
            futures::future::pending::<()>().await;
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn partial_batches_are_not_reported_as_success() {
        use Reply::*;

        for replies in [
            [Status(503), Status(200), Status(200)],
            [Status(200), Status(200), Status(503)],
            [Status(200), Network, Status(200)],
            [Status(200), Status(200), Network],
            [Status(200), Status(200), Timeout],
            [Status(200), Status(200), Body],
            [Status(200), Status(200), Build],
            [Status(200), Status(200), Status(200)],
        ] {
            let endpoint = Endpoint::from_slice("http://localhost/traces");
            let template = create_send_data(1, &endpoint);
            let TracerPayloadCollection::V07(payloads) = template.get_payloads() else {
                panic!("expected V07 payload");
            };
            let mut payload = payloads[0].clone();
            payload.chunks.push(Default::default());
            let mut data = SendData::new(
                3,
                TracerPayloadCollection::V07(vec![payload; 3]),
                Default::default(),
                &endpoint,
            );
            data.set_retry_strategy(RetryStrategy::new(0, 0, RetryBackoffType::Constant, None));
            let client = ScriptedClient(Arc::new(Mutex::new(VecDeque::from(replies))));
            let result = data.send(&client).await;
            assert!(client.0.lock().unwrap().is_empty());
            let success = replies.iter().all(|reply| matches!(reply, Status(200)));
            let dropped = u64::from(!success);
            assert_eq!(result.requests_count, 3);
            assert_eq!(result.chunks_sent, 3 - dropped);
            assert_eq!(result.chunks_dropped, dropped);

            let flusher = TraceFlusher::default();
            let text =
                capture_logs(async { flusher.handle_trace_response(endpoint, result) }).await;

            assert_eq!(
                text.contains("Successfully flushed traces"),
                success,
                "{replies:?}: {text}"
            );
            assert_eq!(
                text.contains("Error sending trace"),
                !success,
                "{replies:?}: {text}"
            );
            assert_eq!(
                flusher.stats().agent_config_writers,
                u32::from(success),
                "{replies:?}"
            );
            let metrics = flusher.collect_metrics();
            assert_eq!(metrics.api_requests, 3);
            assert_eq!(metrics.chunks_sent, 3 - dropped);
            assert_eq!(metrics.chunks_dropped, dropped);
        }
    }

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
