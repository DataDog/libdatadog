// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use libdd_capabilities::{Bytes, HttpError};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::{span::v04::SpanBytes, test_utils::create_test_no_alloc_span};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug, Default)]
struct CancellingClient {
    requests: Arc<AtomicUsize>,
    token: CancellationToken,
    complete: bool,
}

impl HttpClientCapability for CancellingClient {
    fn new_client() -> Self {
        Self::default()
    }
    fn new_periodic() -> Self {
        Self::default()
    }

    async fn request(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, HttpError> {
        if request.method() == http::Method::POST
            && self.requests.fetch_add(1, Ordering::SeqCst) == 1
        {
            self.token.cancel();
            if !self.complete {
                return std::future::pending().await;
            }
        }
        Ok(http::Response::builder()
            .status(200)
            .body(Bytes::from_static(br#"{"rate_by_service":{}}"#))
            .unwrap())
    }
}

impl SleepCapability for CancellingClient {
    fn new() -> Self {
        Self::default()
    }
    async fn sleep(&self, _: std::time::Duration) {
        std::future::pending::<()>().await;
    }
}

impl LogWriterCapability for CancellingClient {
    fn write_log_output(&self, _: &[u8]) -> std::io::Result<()> {
        unreachable!()
    }
}

#[test]
fn cancellation_retains_completed_results_and_distinguishes_unsent_work() {
    let exporter = TraceExporter::<CancellingClient, ForkSafeRuntime>::builder()
        .build::<CancellingClient>()
        .unwrap();
    let token = exporter.capabilities.token.clone();
    let chunks = (1..=3)
        .map(|id| {
            let mut span: SpanBytes = create_test_no_alloc_span(id, id, 0, 1, true);
            span.resource = BytesString::from("x".repeat(600));
            vec![span]
        })
        .collect();
    let result =
        exporter.send_trace_chunks_batched(PooledChunks::unpooled(chunks), 1024, Some(&token));
    assert!(result.error.is_some());
    assert_eq!(result.payloads.len(), 1);
    assert!(result.payloads[0].response.is_ok());
    assert_eq!(result.payloads[0].observations.chunks_sent, 1);
    assert_eq!((result.in_flight_chunks, result.in_flight_spans), (1, 1));
    assert_eq!(
        (result.unattempted_chunks, result.unattempted_spans),
        (1, 1)
    );
    assert_eq!(exporter.capabilities.requests.load(Ordering::SeqCst), 2);
}

#[test]
fn cancellation_with_the_final_response_does_not_fail_completed_work() {
    let mut exporter = TraceExporter::<CancellingClient, ForkSafeRuntime>::builder()
        .build::<CancellingClient>()
        .unwrap();
    exporter.capabilities.complete = true;
    let token = exporter.capabilities.token.clone();
    let traces = (1..=2)
        .map(|id| {
            vec![SpanBytes {
                trace_id: id,
                span_id: u64::try_from(id).unwrap(),
                resource: BytesString::from("x".repeat(600)),
                ..Default::default()
            }]
        })
        .collect();
    let batch =
        exporter.send_trace_chunks_batched(PooledChunks::unpooled(traces), 1024, Some(&token));
    assert!(token.is_cancelled());
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 2);
    assert_eq!((batch.unattempted_chunks, batch.in_flight_chunks), (0, 0));
    assert_eq!((batch.pending_p0_traces, batch.pending_p0_spans), (0, 0));
    assert!(
        batch
            .payloads
            .iter()
            .all(|p| p.observations.chunks_sent == 1)
    );
    assert!(matches!(
        &batch.payloads[1].response,
        Ok(AgentResponse::Changed { .. })
    ));
}

#[derive(Clone, Debug, Default)]
struct RecordingClient(Arc<std::sync::Mutex<Vec<http::Request<Bytes>>>>);

impl HttpClientCapability for RecordingClient {
    fn new_client() -> Self {
        Self::default()
    }
    fn new_periodic() -> Self {
        Self::default()
    }
    async fn request(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, HttpError> {
        let status = if request.uri().path() == "/v1.0/traces" {
            404
        } else {
            200
        };
        let body = if request.method() == http::Method::GET {
            Bytes::from_static(
                br#"{"client_drop_p0s":true,"endpoints":["/v0.6/stats","/v1.0/traces"]}"#,
            )
        } else {
            self.0.lock().unwrap().push(request);
            Bytes::from_static(br#"{"rate_by_service":{}}"#)
        };
        Ok(http::Response::builder().status(status).body(body).unwrap())
    }
}
impl SleepCapability for RecordingClient {
    fn new() -> Self {
        Self::default()
    }
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
impl LogWriterCapability for RecordingClient {
    fn write_log_output(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn p0_accounting_survives_empty_and_all_oversized_selections() {
    use libdd_shared_runtime::{SharedRuntime, Worker};
    let runtime = ForkSafeRuntime::new().unwrap();
    runtime
        .block_on(async {
            let (mut fetcher, _) = crate::agent_info::AgentInfoFetcher::<RecordingClient>::new(
                Endpoint::from_slice("http://localhost:8126/info"),
                Duration::from_secs(300),
            );
            fetcher.run().await;
        })
        .unwrap();
    for oversized in [false, true] {
        let mut builder = TraceExporter::<RecordingClient, ForkSafeRuntime>::builder();
        builder
            .enable_stats(Duration::from_secs(3600))
            .enable_observations();
        let exporter = builder.build::<RecordingClient>().unwrap();
        let mut dropped = SpanBytes {
            trace_id: 1,
            span_id: 1,
            ..Default::default()
        };
        dropped.metrics.insert("_sampling_priority_v1".into(), 0.0);
        let mut chunks = vec![vec![dropped]];
        if oversized {
            let mut span = SpanBytes {
                trace_id: 2,
                span_id: 2,
                resource: BytesString::from("x".repeat(2048)),
                ..Default::default()
            };
            span.metrics.insert("_sampling_priority_v1".into(), 1.0);
            chunks.push(vec![span]);
        }
        let batch = exporter.send_trace_chunks_batched(PooledChunks::unpooled(chunks), 1024, None);
        assert!(batch.error.is_none());
        assert_eq!(batch.payloads.len(), 1);
        assert_eq!(batch.payloads[0].chunks, 0);
        assert_eq!(batch.oversized_chunks, usize::from(oversized));
        assert_eq!(batch.pending_p0_traces, 0);
        let requests = exporter.capabilities.0.lock().unwrap();
        let requests: Vec<_> = requests
            .iter()
            .filter(|r| r.uri().path() == "/v0.4/traces")
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body().as_ref(), &[0x90]);
        assert_eq!(
            requests[0].headers()["datadog-client-dropped-p0-traces"],
            "1"
        );
        assert_eq!(
            requests[0].headers()["datadog-client-dropped-p0-spans"],
            "1"
        );
    }
}

static INNER: std::sync::OnceLock<TraceExporter<RecordingClient, ForkSafeRuntime>> =
    std::sync::OnceLock::new();

#[derive(Clone, Debug, Default)]
struct ReentrantClient;
impl HttpClientCapability for ReentrantClient {
    fn new_client() -> Self {
        Self
    }
    fn new_periodic() -> Self {
        Self
    }
    async fn request(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, HttpError> {
        if request.method() == http::Method::POST {
            let response = INNER
                .get()
                .unwrap()
                .send_trace_chunks_async(PooledChunks::unpooled(vec![vec![SpanBytes::default()]]))
                .await
                .unwrap();
            assert!(matches!(response, AgentResponse::Changed { .. }));
        }
        RecordingClient::default().request(request).await
    }
}
impl SleepCapability for ReentrantClient {
    fn new() -> Self {
        Self
    }
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
impl LogWriterCapability for ReentrantClient {
    fn write_log_output(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn nested_legacy_sends_do_not_change_batch_results() {
    let inner = TraceExporter::<RecordingClient, ForkSafeRuntime>::builder()
        .build::<RecordingClient>()
        .unwrap();
    assert!(INNER.set(inner).is_ok());
    let exporter = TraceExporter::<ReentrantClient, ForkSafeRuntime>::builder()
        .build::<ReentrantClient>()
        .unwrap();
    let chunks = (1..=2)
        .map(|id| {
            vec![SpanBytes {
                trace_id: id,
                span_id: u64::try_from(id).unwrap(),
                ..Default::default()
            }]
        })
        .collect();
    let batch = exporter.send_trace_chunks_batched(PooledChunks::unpooled(chunks), 130, None);
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 2);
    assert_eq!(batch.payloads.iter().map(|p| p.chunks).sum::<usize>(), 2);
    assert_eq!(batch.unattempted_chunks, 0);
    assert_eq!(batch.in_flight_chunks, 0);
    assert!(
        batch
            .payloads
            .iter()
            .all(|p| p.observations.requests_count == 1)
    );
}

#[test]
fn timerless_runtimes_use_the_supplied_sleep_capability() {
    use libdd_shared_runtime::BasicRuntime;
    let runtime = Arc::new(BasicRuntime::from_handle(Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap(),
    )));
    let mut builder = TraceExporter::<CancellingClient, BasicRuntime>::builder();
    builder.set_shared_runtime(runtime);
    let exporter = builder.build::<CancellingClient>().unwrap();
    let batch = exporter.send_trace_chunks_batched(
        PooledChunks::unpooled(vec![vec![SpanBytes::default()]]),
        1024,
        None,
    );
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 1);
    assert!(batch.payloads[0].response.is_ok());
}

#[test]
fn feedback_validation_skips_unrelated_json_without_building_a_tree() {
    let body = format!(
        r#"{{"rate_by_service":{{}},"unrelated":[{}0]}}"#,
        "0,".repeat(4_000_000)
    );
    assert!(has_sampling_feedback(body.as_bytes()));
    assert!(!has_sampling_feedback(br#"{"rate_by_service":[]}"#));
    assert!(!has_sampling_feedback(b"[{}]"));
    assert!(!has_sampling_feedback(
        br#"{"rate_by_service":{}} trailing"#
    ));
}

#[test]
fn p0_counts_remain_pending_after_a_request_build_failure() {
    use libdd_shared_runtime::{SharedRuntime, Worker};
    let runtime = ForkSafeRuntime::new().unwrap();
    runtime
        .block_on(async {
            let (mut fetcher, _) = crate::agent_info::AgentInfoFetcher::<RecordingClient>::new(
                Endpoint::from_slice("http://localhost:8126/info"),
                Duration::from_secs(300),
            );
            fetcher.run().await;
        })
        .unwrap();
    let mut builder = TraceExporter::<RecordingClient, ForkSafeRuntime>::builder();
    builder
        .enable_stats(Duration::from_secs(3600))
        .enable_observations();
    let mut exporter = builder.build::<RecordingClient>().unwrap();
    exporter.endpoint.api_key = Some("invalid\nheader".into());
    let mut span = SpanBytes {
        trace_id: 1,
        span_id: 1,
        ..Default::default()
    };
    span.metrics.insert("_sampling_priority_v1".into(), 0.0);
    let result =
        exporter.send_trace_chunks_batched(PooledChunks::unpooled(vec![vec![span]]), 1024, None);
    assert_eq!(result.pending_p0_traces, 1);
    assert_eq!(result.pending_p0_spans, 1);
    assert_eq!(result.payloads.len(), 1);
    assert!(result.payloads[0].response.is_err());
    assert!(exporter.capabilities.0.lock().unwrap().is_empty());
}

#[test]
fn a_rejected_p0_control_request_disables_v1_for_the_next_batch() {
    use libdd_shared_runtime::{SharedRuntime, Worker};
    let runtime = ForkSafeRuntime::new().unwrap();
    runtime
        .block_on(async {
            let (mut fetcher, _) = crate::agent_info::AgentInfoFetcher::<RecordingClient>::new(
                Endpoint::from_slice("http://localhost:8126/info"),
                Duration::from_secs(300),
            );
            fetcher.run().await;
        })
        .unwrap();
    let mut builder = TraceExporter::<RecordingClient, ForkSafeRuntime>::builder();
    builder
        .enable_v1_protocol()
        .enable_stats(Duration::from_secs(3600));
    let exporter = builder.build::<RecordingClient>().unwrap();
    let mut span = SpanBytes {
        trace_id: 1,
        span_id: 1,
        ..Default::default()
    };
    span.metrics.insert("_sampling_priority_v1".into(), 0.0);
    let batch =
        exporter.send_trace_chunks_batched(PooledChunks::unpooled(vec![vec![span]]), 1024, None);
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 1);
    assert_eq!(batch.payloads[0].chunks, 0);
    assert!(batch.payloads[0].response.is_err());
    assert!(!exporter.v1_active.load(Ordering::Relaxed));
    let mut span = SpanBytes {
        trace_id: 2,
        span_id: 2,
        ..Default::default()
    };
    span.metrics.insert("_sampling_priority_v1".into(), 1.0);
    let batch =
        exporter.send_trace_chunks_batched(PooledChunks::unpooled(vec![vec![span]]), 1024, None);
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 1);
    assert!(batch.payloads[0].response.is_ok());
    let requests = exporter.capabilities.0.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.uri().path() == "/v1.0/traces")
            .count(),
        6
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.uri().path() == "/v0.4/traces")
            .count(),
        1
    );
}
