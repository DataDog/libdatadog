// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the agentless stats export feature.
//!
//! Each test runs inside `spawn_blocking` (required because `send_trace_chunks` calls `block_on`)
//! and calls `mock.register_on_current_thread()` before `build()` so the builder-internal
//! capabilities share the mock queues. `shutdown()` force-flushes the stats worker.
//!
//! `STATS_BUCKET` is large so the periodic flush never fires during a test; shutdown is the
//! only flush that occurs, keeping tests deterministic.

mod common;
use common::mock_http::MockHttpCapabilities;

use libdd_data_pipeline::trace_exporter::TraceExporterBuilder;
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_tinybytes::BytesString;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::v04::{SpanBytes, VecMap};
use std::time::Duration;
use tokio::task;

/// Large enough that the periodic flush never fires during a test; only shutdown flushes.
const STATS_BUCKET: Duration = Duration::from_secs(10);

/// Build a root `SpanBytes` (parent_id = 0, so `compute_top_level_span` marks it top-level).
///
/// * `trace_id` — lets callers create distinct operations in the concentrator.
/// * `sampling_priority` — sets `_sampling_priority_v1`; `None` omits the metric (treated as
///   positive by `drop_chunks`).
/// * `error` — `1` triggers the error-sampler path in `drop_chunks`, keeping the chunk even when
///   the priority is negative.
fn make_root_span(trace_id: u128, sampling_priority: Option<f64>, error: i32) -> SpanBytes {
    SpanBytes {
        service: BytesString::from_static("test-svc"),
        name: BytesString::from_static("test-op"),
        resource: BytesString::from_static("GET /"),
        trace_id,
        span_id: 1,
        parent_id: 0,
        start: 1_000_000_000,
        duration: 5_000_000,
        error,
        metrics: VecMap::from_iter(
            sampling_priority.map(|p| (BytesString::from_static("_sampling_priority_v1"), p)),
        ),
        ..Default::default()
    }
}

/// Build a single exporter, call `send_trace_chunks` for each element of `chunks_per_call`,
/// shut down, and return all captured requests.
///
/// Both the trace intake and the stats endpoint are wired through `mock`.
async fn run_agentless_with_stats(
    mock: &MockHttpCapabilities,
    chunks_per_call: Vec<Vec<Vec<SpanBytes>>>,
    service: &'static str,
) -> Vec<common::mock_http::CapturedRequest> {
    let mock_clone = mock.clone();
    task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_agentless_endpoint("https://traces.fake.example.com/v1/input", "test-api-key")
            .set_agentless_stats_endpoint("https://stats.fake.example.com/api/v0.2/stats")
            .enable_stats(STATS_BUCKET)
            .set_language("rust")
            .set_language_version("1.85")
            .set_language_interpreter("rustc")
            .set_tracer_version("0.0.0-test")
            .set_env("integration-test")
            .set_service(service)
            .set_hostname("test-host");

        let exporter = builder
            .build::<MockHttpCapabilities>()
            .expect("TraceExporter::build failed");

        for chunks in chunks_per_call {
            exporter
                .send_trace_chunks(chunks, None)
                .expect("send_trace_chunks failed");
        }
        exporter.shutdown(None).expect("shutdown failed");
    })
    .await
    .expect("spawn_blocking panicked");

    mock.captured_requests()
}

/// Decode the JSON body of an agentless `/v1/input` request and return the total number of
/// spans across all trace chunks.
fn count_spans_in_intake_request(req: &common::mock_http::CapturedRequest) -> usize {
    let body: serde_json::Value =
        serde_json::from_slice(&req.body).expect("/v1/input body must be valid JSON");
    body["traces"]
        .as_array()
        .expect("`traces` array missing")
        .iter()
        .map(|trace| trace["spans"].as_array().map_or(0, |s| s.len()))
        .sum()
}

/// Decode the stats payload from a `/api/v0.2/stats` request and return a flattened list of
/// all `ClientGroupedStats` entries across every bucket.
fn grouped_stats_from_request(
    req: &common::mock_http::CapturedRequest,
) -> Vec<pb::ClientGroupedStats> {
    let payload: pb::StatsPayload =
        rmp_serde::from_slice(&req.body).expect("stats body must be valid msgpack StatsPayload");
    payload
        .stats
        .into_iter()
        .flat_map(|csp| csp.stats)
        .flat_map(|bucket| bucket.stats)
        .collect()
}

/// When `set_agentless_stats_endpoint` is configured alongside
/// `set_agentless_endpoint` and `enable_stats`, sending a trace must produce:
///
/// 1. A POST to the trace intake URL.
/// 2. A POST to the stats intake URL (on shutdown force-flush).
///
/// The stats request must carry the `dd-api-key` header and
/// `Content-Type: application/msgpack`.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_agentless_stats_sent_to_correct_endpoint() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/input", 200, "{}");
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    let mock_clone = mock.clone();
    task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_agentless_endpoint(
                "https://traces.fake.example.com/v1/input",
                "my-test-api-key",
            )
            .set_agentless_stats_endpoint("https://stats.fake.example.com/api/v0.2/stats")
            .enable_stats(STATS_BUCKET)
            .set_language("rust")
            .set_language_version("1.85")
            .set_language_interpreter("rustc")
            .set_tracer_version("0.0.0-test")
            .set_env("integration-test")
            .set_service("test-svc")
            .set_hostname("test-host");

        let exporter = builder
            .build::<MockHttpCapabilities>()
            .expect("TraceExporter::build failed");

        exporter
            .send_trace_chunks(vec![vec![make_root_span(1, Some(1.0), 0)]], None)
            .expect("send_trace_chunks failed");
        exporter.shutdown(None).expect("shutdown failed");
    })
    .await
    .expect("spawn_blocking panicked");

    let reqs = mock.captured_requests();
    assert_eq!(
        reqs.len(),
        2,
        "expected 2 requests (trace + stats), got {}; requests: {:#?}",
        reqs.len(),
        reqs.iter()
            .map(|r| format!("{} {}", r.method, r.uri))
            .collect::<Vec<_>>()
    );

    let trace_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/input")
        .expect("trace intake request not found");
    assert_eq!(trace_req.method, http::Method::POST);

    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("stats intake request not found");
    assert_eq!(stats_req.method, http::Method::POST);
    assert_eq!(
        stats_req.header("dd-api-key"),
        "my-test-api-key",
        "stats request must carry the agentless API key"
    );
    assert_eq!(
        stats_req.header("content-type"),
        "application/msgpack",
        "stats payload must be msgpack"
    );
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_agentless_stats_payload_structure() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/input", 200, "{}");
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    let mock_clone = mock.clone();
    task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_agentless_endpoint(
                "https://traces.fake.example.com/v1/input",
                "payload-test-key",
            )
            .set_agentless_stats_endpoint("https://stats.fake.example.com/api/v0.2/stats")
            .enable_stats(STATS_BUCKET)
            .set_language("python")
            .set_language_version("1.85")
            .set_language_interpreter("rustc")
            .set_tracer_version("1.2.3")
            .set_env("prod")
            .set_service("payload-svc")
            .set_hostname("payload-host");

        let exporter = builder
            .build::<MockHttpCapabilities>()
            .expect("build failed");

        exporter
            .send_trace_chunks(vec![vec![make_root_span(1, Some(1.0), 0)]], None)
            .expect("send_trace_chunks failed");
        exporter.shutdown(None).expect("shutdown failed");
    })
    .await
    .expect("spawn_blocking panicked");

    let reqs = mock.captured_requests();
    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("stats request not found");

    let payload: pb::StatsPayload = rmp_serde::from_slice(&stats_req.body)
        .expect("stats body must be valid msgpack StatsPayload");

    assert_eq!(payload.agent_hostname, "payload-host");
    assert_eq!(payload.agent_env, "prod");
    assert!(
        payload.client_computed,
        "client_computed must be true for agentless stats"
    );
    assert!(!payload.split_payload, "split_payload must be false");
    assert_eq!(
        payload.stats.len(),
        1,
        "expected exactly one ClientStatsPayload"
    );

    let client_payload = &payload.stats[0];
    assert_eq!(client_payload.service, "payload-svc");
    assert!(
        !client_payload.stats.is_empty(),
        "must contain at least one stats bucket"
    );

    assert_eq!(
        client_payload.stats[0].stats[0].hits, 1,
        "stats must have one hit"
    );

    // The agentless path must populate lang / tracer_version (the Agent no
    // longer enriches the payload downstream).
    assert_eq!(
        client_payload.lang, "python",
        "lang must be set on the client payload in agentless mode"
    );
    assert_eq!(
        client_payload.tracer_version, "1.2.3",
        "tracer_version must be set on the client payload in agentless mode"
    );

    assert_eq!(
        payload.agent_version, "1.2.3-python",
        "agent_version must be '{{tracer_version}}-{{language}}'"
    );
}

/// A span with positive sampling priority (`_sampling_priority_v1 = 1`) must
/// be forwarded to `/v1/input`.  The body must contain exactly one span and
/// the stats bucket must record `hits = 1, errors = 0`.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_sampled_span_sent_to_intake_with_correct_stats() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/input", 200, "{}");
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    let reqs = run_agentless_with_stats(
        &mock,
        vec![vec![vec![make_root_span(1, Some(1.0), 0)]]],
        "test-svc",
    )
    .await;

    // One trace POST + one stats POST.
    assert_eq!(
        reqs.len(),
        2,
        "expected trace + stats requests, got {}: {:#?}",
        reqs.len(),
        reqs.iter().map(|r| r.uri.to_string()).collect::<Vec<_>>()
    );

    // The trace request must contain exactly 1 span.
    let trace_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/input")
        .expect("/v1/input request not found");
    assert_eq!(
        count_spans_in_intake_request(trace_req),
        1,
        "sampled span must be forwarded to the intake"
    );

    // Stats must record 1 hit, 0 errors.
    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("/api/v0.2/stats request not found");
    let groups = grouped_stats_from_request(stats_req);
    let total_hits: u64 = groups.iter().map(|g| g.hits).sum();
    let total_errors: u64 = groups.iter().map(|g| g.errors).sum();
    assert_eq!(
        total_hits, 1,
        "stats must record 1 hit for the sampled span"
    );
    assert_eq!(total_errors, 0, "stats must record 0 errors");
}

/// A span with a **negative** sampling priority and no error must be dropped
/// by `drop_chunks` after stats are recorded.  The exporter must therefore
/// skip the `/v1/input` POST entirely and only send the stats flush.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_p0_span_not_sent_to_intake_but_counted_in_stats() {
    let mock = MockHttpCapabilities::new();
    // No response queued for /v1/input — any POST there would be unexpected.
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    let reqs = run_agentless_with_stats(
        &mock,
        vec![vec![vec![make_root_span(1, Some(-1.0), 0)]]],
        "test-svc",
    )
    .await;

    // Only the stats flush; no trace POST because all chunks were dropped.
    assert!(
        reqs.iter().all(|r| r.uri.path() != "/v1/input"),
        "a dropped P0 span must not be forwarded to /v1/input; requests: {:#?}",
        reqs.iter().map(|r| r.uri.to_string()).collect::<Vec<_>>()
    );

    // Stats must still count the dropped span.
    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("/api/v0.2/stats request not found");
    let groups = grouped_stats_from_request(stats_req);
    let total_hits: u64 = groups.iter().map(|g| g.hits).sum();
    assert_eq!(
        total_hits, 1,
        "dropped P0 span must still be counted in stats"
    );
}

/// A span with a **negative** sampling priority but `error = 1` must be kept
/// by the error-sampler path in `drop_chunks` and forwarded to `/v1/input`.
/// Stats must record `hits = 1, errors = 1`.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_error_span_with_negative_priority_kept_by_error_sampler() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/input", 200, "{}");
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    let reqs = run_agentless_with_stats(
        &mock,
        vec![vec![vec![make_root_span(1, Some(-1.0), 1)]]],
        "test-svc",
    )
    .await;

    // The error chunk must reach the intake.
    let trace_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/input")
        .expect("error span must be forwarded to /v1/input despite negative priority");
    assert_eq!(
        count_spans_in_intake_request(trace_req),
        1,
        "exactly one span must be in the intake request"
    );

    // Stats: 1 hit, 1 error.
    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("/api/v0.2/stats not found");
    let groups = grouped_stats_from_request(stats_req);
    let total_hits: u64 = groups.iter().map(|g| g.hits).sum();
    let total_errors: u64 = groups.iter().map(|g| g.errors).sum();
    assert_eq!(total_hits, 1, "stats must record 1 hit");
    assert_eq!(total_errors, 1, "stats must record 1 error");
}

/// Sending **two** trace chunks in separate calls — one sampled (`priority = 1`) and one dropped
/// (`priority = -1`, no error) — must forward only the sampled chunk to `/v1/input`,
/// while the stats bucket records `hits = 2` (both counted before dropping).
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_mixed_sampling_priorities_span_count_and_stats() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/input", 200, "{}");
    mock.queue_response_for_path("/api/v0.2/stats", 202, "");

    // Two separate send_trace_chunks calls: one sampled (trace_id=1), one dropped (trace_id=2).
    let reqs = run_agentless_with_stats(
        &mock,
        vec![
            vec![vec![make_root_span(1, Some(1.0), 0)]],
            vec![vec![make_root_span(2, Some(-1.0), 0)]],
        ],
        "test-svc",
    )
    .await;

    // Exactly one POST to /v1/input (the sampled trace only).
    let trace_reqs: Vec<_> = reqs
        .iter()
        .filter(|r| r.uri.path() == "/v1/input")
        .collect();
    assert_eq!(
        trace_reqs.len(),
        1,
        "only the sampled trace must reach /v1/input"
    );
    assert_eq!(
        count_spans_in_intake_request(trace_reqs[0]),
        1,
        "the /v1/input request must contain exactly 1 span (the sampled one)"
    );

    // Stats must count both spans (sampled + dropped).
    let stats_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/api/v0.2/stats")
        .expect("/api/v0.2/stats not found");
    let groups = grouped_stats_from_request(stats_req);
    let total_hits: u64 = groups.iter().map(|g| g.hits).sum();
    assert_eq!(
        total_hits, 2,
        "stats must count both the sampled and dropped span (got {total_hits})"
    );
}

/// `set_agentless_stats_endpoint` without `set_agentless_endpoint` must be
/// rejected at build time.
#[cfg_attr(miri, ignore)]
#[test]
fn test_builder_rejects_agentless_stats_without_agentless_traces() {
    let mock = MockHttpCapabilities::new();
    mock.register_on_current_thread();

    let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
    builder
        .set_agentless_stats_endpoint("https://stats.fake.example.com/api/v0.2/stats")
        .enable_stats(STATS_BUCKET);

    let err = builder
        .build::<MockHttpCapabilities>()
        .expect_err("should fail without agentless trace endpoint");

    let msg = err.to_string();
    assert!(
        msg.contains("agentless stats") || msg.contains("agentless trace"),
        "unexpected error message: {msg}"
    );
}

/// `set_agentless_stats_endpoint` combined with `set_otlp_metrics_endpoint`
/// must be rejected at build time.
#[cfg_attr(miri, ignore)]
#[test]
fn test_builder_rejects_agentless_stats_with_otlp_stats() {
    let mock = MockHttpCapabilities::new();
    mock.register_on_current_thread();

    let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
    builder
        .set_agentless_endpoint("https://traces.fake.example.com/v1/input", "key")
        .set_agentless_stats_endpoint("https://stats.fake.example.com/api/v0.2/stats")
        .set_otlp_metrics_endpoint("http://otel.fake.example.com/v1/metrics")
        .enable_stats(STATS_BUCKET);

    let err = builder
        .build::<MockHttpCapabilities>()
        .expect_err("should fail when both agentless stats and OTLP stats are set");

    let msg = err.to_string();
    assert!(
        msg.contains("agentless stats") && msg.contains("OTLP"),
        "unexpected error message: {msg}"
    );
}
