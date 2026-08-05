// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the agentless stats export feature.
//!
//! Each test runs inside `spawn_blocking` (required because `send`/`shutdown` call `block_on`)
//! and calls `mock.register_on_current_thread()` before `build()` so the builder-internal
//! capabilities share the mock queues. `shutdown()` force-flushes the stats worker.
//!
//! `STATS_BUCKET` is large so the periodic flush never fires during a test; shutdown is the
//! only flush that occurs, keeping tests deterministic.

mod common;
use common::mock_http::MockHttpCapabilities;

use libdd_data_pipeline::trace_exporter::{TraceExporterBuilder, TraceExporterInputFormat};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_protobuf::pb;
use serde_json::json;
use std::time::Duration;
use tokio::task;

/// Large enough that the periodic flush never fires during a test; only shutdown flushes.
const STATS_BUCKET: Duration = Duration::from_secs(10);

/// Build a msgpack-encoded V04 trace payload containing one sampled root span.
///
/// * `parent_id = 0` — root span, so `compute_top_level_span` marks it as top-level, causing the
///   stats concentrator to record a bucket entry.
/// * `_sampling_priority_v1 = 1` — positive priority, so `drop_chunks` keeps the trace when
///   agentless stats are enabled and P0 dropping is active.
fn make_v04_trace_payload() -> Vec<u8> {
    let span = json!({
        "trace_id": 1u64,
        "span_id": 1u64,
        "parent_id": 0u64,
        "service": "test-svc",
        "name": "test-op",
        "resource": "GET /",
        "start": 1_000_000_000i64,
        "duration": 5_000_000i64,
        "error": 0,
        "meta": { "env": "integration-test" },
        // Positive sampling priority: kept by drop_chunks when stats are enabled.
        "metrics": { "_sampling_priority_v1": 1.0 },
        "meta_struct": {},
        "span_links": [],
        "span_events": [],
    });
    rmp_serde::to_vec_named(&vec![vec![span]]).expect("msgpack encode")
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

    let trace_payload = make_v04_trace_payload();

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

        let _ = exporter.send(&trace_payload).expect("send failed");
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

    let trace_payload = make_v04_trace_payload();

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

        let _ = exporter.send(&trace_payload).expect("send failed");
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

    assert!(
        payload.agent_version.contains("-libdatadog"),
        "agent_version ({}) must contain the '-libdatadog' suffix",
        payload.agent_version
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

/// Smoke-test that `MockHttpCapabilities` intercepts requests in agent (non-agentless) mode.
/// Only verifies the trace POST arrives; full stats-enable coverage is in `test_trace_exporter.rs`.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_agent_mode_trace_request_captured() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path(
        "/info",
        200,
        r#"{"version":"7.0.0","endpoints":["/v0.4/traces"]}"#,
    );
    mock.queue_response_for_path("/v0.4/traces", 200, r#"{"rate_by_service":{}}"#);

    let trace_payload = make_v04_trace_payload();

    let mock_clone = mock.clone();
    task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_url("http://fake-agent.example.com")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_language("rust")
            .set_language_version("1.85")
            .set_language_interpreter("rustc")
            .set_tracer_version("0.0.0-test")
            .set_env("integration-test")
            .set_service("test-svc");

        let exporter = builder
            .build::<MockHttpCapabilities>()
            .expect("build failed");

        let _ = exporter.send(&trace_payload);
        exporter.shutdown(None).expect("shutdown failed");
    })
    .await
    .expect("spawn_blocking panicked");

    let arrived = mock.wait_for_requests(1, Duration::from_secs(2)).await;
    assert!(arrived, "no requests captured at all");

    let reqs = mock.captured_requests();
    let trace_req = reqs
        .iter()
        .find(|r| r.method == http::Method::POST)
        .expect("no POST request captured");
    assert!(
        trace_req.uri.path().contains("traces"),
        "expected a /traces POST, got {}",
        trace_req.uri
    );
}
