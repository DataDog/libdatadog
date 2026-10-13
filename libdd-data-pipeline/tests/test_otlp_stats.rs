// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the OTLP trace-metrics stats flusher
//! (`traces.span.sdk.metrics.duration` histograms sent to the OTLP metrics endpoint).
//!
//! Each test runs inside `spawn_blocking` (required because `send_trace_chunks` and
//! `flush_client_side_stats` call `block_on`) and calls `mock.register_on_current_thread()`
//! before `build()` so the builder-internal capabilities share the mock queues.
//!
//! `STATS_BUCKET` is large so the periodic flush never fires during a test. Only explicit
//! flushes occur (shutdown, or `flush_client_side_stats` in the force-flush test), keeping
//! tests deterministic.

mod common;
use common::mock_http::MockHttpCapabilities;

use libdd_data_pipeline::trace_exporter::TraceExporterBuilder;
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::{
    span_pool::PooledChunks,
    v04::{SpanBytes, VecMap},
};
use serde_json::Value;
use std::time::Duration;
use tokio::task;

/// Large enough that the periodic flush never fires during a test; only explicit flushes fire.
const STATS_BUCKET: Duration = Duration::from_secs(10);

const TRACES_ENDPOINT: &str = "https://otlp-traces.fake.example.com/v1/traces";
const METRICS_ENDPOINT: &str = "https://otlp-metrics.fake.example.com/v1/metrics";
const METRIC_NAME: &str = "traces.span.sdk.metrics.duration";

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

/// Find the `/v1/metrics` request in `reqs` and decode it as a valid OTLP
/// `ExportMetricsServiceRequest` (JSON), proving a real export path ran rather than a no-op.
fn expect_metrics_request(reqs: &[common::mock_http::CapturedRequest]) -> Value {
    let req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/metrics")
        .expect("a request to the OTLP metrics endpoint should have been sent");
    serde_json::from_slice(&req.body).expect("metrics body must be valid OTLP JSON")
}

/// Extract the histogram of the (single) `traces.span.sdk.metrics.duration` metric from an
/// OTLP metrics request, asserting the payload carries exactly one metric and one data point.
fn single_data_point(body: &Value) -> &Value {
    assert_eq!(
        body["resourceMetrics"].as_array().map(Vec::len),
        Some(1),
        "expected exactly one resourceMetrics entry: {body:#?}"
    );
    let metrics = &body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"];
    let metrics = metrics
        .as_array()
        .expect("scopeMetrics[0].metrics must be an array");
    assert_eq!(metrics.len(), 1, "expected exactly one metric");
    assert_eq!(
        metrics[0]["name"], METRIC_NAME,
        "the metric must be the OTLP trace-metrics duration histogram"
    );
    assert_eq!(
        metrics[0]["unit"], "s",
        "the duration histogram must be expressed in seconds"
    );
    let histogram = &metrics[0]["histogram"];
    assert_eq!(
        histogram["aggregationTemporality"], 1,
        "delta temporality: each export covers only its own interval"
    );
    let data_points = histogram["dataPoints"]
        .as_array()
        .expect("histogram.dataPoints must be an array");
    assert_eq!(
        data_points.len(),
        1,
        "one eligible span must produce exactly one (ok) data point"
    );
    &data_points[0]
}

/// Return the value of the string attribute `key` on an OTLP data point.
fn str_attribute(data_point: &Value, key: &str) -> String {
    data_point["attributes"]
        .as_array()
        .expect("data point must carry attributes")
        .iter()
        .find(|attr| attr["key"] == key)
        .unwrap_or_else(|| panic!("attribute '{key}' missing: {data_point:#?}"))["value"]
        ["stringValue"]
        .as_str()
        .unwrap_or_else(|| panic!("attribute '{key}' must be a stringValue"))
        .to_owned()
}

/// Return whether the bool attribute `key` on an OTLP data point is true.
fn bool_attribute_is_true(data_point: &Value, key: &str) -> bool {
    data_point["attributes"]
        .as_array()
        .expect("data point must carry attributes")
        .iter()
        .find(|attr| attr["key"] == key)
        .unwrap_or_else(|| panic!("attribute '{key}' missing: {data_point:#?}"))["value"]
        ["boolValue"]
        .as_bool()
        .unwrap_or_else(|| panic!("attribute '{key}' must be a boolValue"))
}

/// When `set_otlp_metrics_endpoint` is configured alongside `set_otlp_endpoint` and
/// `enable_stats`, sending a trace must produce:
///
/// 1. A POST of the trace to the OTLP trace endpoint, carrying the `datadog-client-computed-stats:
///    yes` header (OTLP stats bypass the Agent, so the header is the only downstream signal that
///    stats were computed client-side).
/// 2. A POST of the `traces.span.sdk.metrics.duration` histogram to the OTLP metrics endpoint (on
///    shutdown force-flush), as `application/json`.
///
/// The histogram data point must carry exact count/sum/min/max and the Datadog attributes of
/// the aggregation key.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_otlp_stats_flush_on_shutdown_sends_valid_metrics() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/traces", 200, "{}");
    mock.queue_response_for_path("/v1/metrics", 200, "");

    let mock_clone = mock.clone();
    task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_otlp_endpoint(TRACES_ENDPOINT)
            .set_otlp_metrics_endpoint(METRICS_ENDPOINT)
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
            .expect("build failed");

        exporter
            .send_trace_chunks(
                PooledChunks::unpooled(vec![vec![make_root_span(1, Some(1.0), 0)]]),
                None,
            )
            .expect("send_trace_chunks failed");
        exporter.shutdown(None).expect("shutdown failed");
    })
    .await
    .expect("spawn_blocking panicked");

    let reqs = mock.captured_requests();

    let trace_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/traces")
        .expect("OTLP trace request not found");
    assert_eq!(trace_req.method, http::Method::POST);
    assert_eq!(
        trace_req.header("datadog-client-computed-stats"),
        "yes",
        "the trace request must announce client-computed stats"
    );

    let metrics_req = reqs
        .iter()
        .find(|r| r.uri.path() == "/v1/metrics")
        .expect("OTLP metrics request not found");
    assert_eq!(metrics_req.method, http::Method::POST);
    assert_eq!(
        metrics_req.header("content-type"),
        "application/json",
        "OTLP metrics must be sent as JSON"
    );

    let body = expect_metrics_request(&reqs);
    let point = single_data_point(&body);
    // One top-level span of 5ms: exact count/sum/min/max, projected into explicit buckets.
    assert_eq!(point["count"], "1", "one span must be counted");
    let five_ms = 5_000_000.0 / 1_000_000_000.0;
    assert_eq!(point["sum"].as_f64(), Some(five_ms));
    assert_eq!(point["min"].as_f64(), Some(five_ms));
    assert_eq!(point["max"].as_f64(), Some(five_ms));
    assert_eq!(
        point["explicitBounds"].as_array().map(Vec::len),
        Some(16),
        "explicit bounds must mirror the spanmetrics-connector defaults"
    );
    let bucket_counts: Vec<u64> = point["bucketCounts"]
        .as_array()
        .expect("bucketCounts must be an array")
        .iter()
        .map(|c| {
            c.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| c.as_u64())
                .unwrap_or_else(|| panic!("bucket count must be numeric: {c}"))
        })
        .collect();
    assert_eq!(bucket_counts.len(), 17, "one count per bound + overflow");
    assert_eq!(
        bucket_counts.iter().sum::<u64>(),
        1,
        "the single span must land in exactly one bucket"
    );

    // Datadog attributes of the aggregation key.
    assert_eq!(str_attribute(point, "service.name"), "test-svc");
    assert_eq!(str_attribute(point, "span.name"), "GET /");
    assert_eq!(str_attribute(point, "datadog.operation.name"), "test-op");
    assert_eq!(str_attribute(point, "status.code"), "STATUS_CODE_OK");
    assert!(
        bool_attribute_is_true(point, "datadog.span.top_level"),
        "the root span must be flagged top-level"
    );

    // Resource attributes.
    let resource_attrs = &body["resourceMetrics"][0]["resource"]["attributes"];
    let find_attr = |key: &str| {
        resource_attrs
            .as_array()
            .expect("resource attributes must be an array")
            .iter()
            .find(|attr| attr["key"] == key)
            .unwrap_or_else(|| panic!("resource attribute '{key}' missing: {resource_attrs:#?}"))
    };
    assert_eq!(
        find_attr("telemetry.sdk.name")["value"]["stringValue"],
        "datadog"
    );
    assert_eq!(
        find_attr("telemetry.sdk.language")["value"]["stringValue"],
        "rust"
    );
    assert_eq!(
        find_attr("telemetry.sdk.version")["value"]["stringValue"],
        "0.0.0-test"
    );
}

/// `TraceExporter::flush_client_side_stats` must trigger an immediate forced flush of the OTLP
/// stats exporter through the `Weak<dyn FlushableStatsExport>` handle stored in
/// `StatsComputationStatus::Enabled`, without waiting for the periodic worker or shutdown.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_flush_client_side_stats_sends_otlp_metrics() {
    let mock = MockHttpCapabilities::new();
    mock.queue_response_for_path("/v1/traces", 200, "{}");
    // One response for the explicit flush; an extra one in case the shutdown flush also fires.
    mock.queue_response_for_path("/v1/metrics", 200, "");
    mock.queue_response_for_path("/v1/metrics", 200, "");

    let mock_clone = mock.clone();
    let (flushed, before_shutdown) = task::spawn_blocking(move || {
        mock_clone.register_on_current_thread();

        let mut builder = TraceExporterBuilder::<ForkSafeRuntime>::new();
        builder
            .set_otlp_endpoint(TRACES_ENDPOINT)
            .set_otlp_metrics_endpoint(METRICS_ENDPOINT)
            .enable_stats(STATS_BUCKET)
            .set_language("rust")
            .set_tracer_version("0.0.0-test")
            .set_env("integration-test")
            .set_service("test-svc")
            .set_hostname("test-host");

        let exporter = builder
            .build::<MockHttpCapabilities>()
            .expect("build failed");

        exporter
            .send_trace_chunks(
                PooledChunks::unpooled(vec![vec![make_root_span(1, Some(1.0), 0)]]),
                None,
            )
            .expect("send_trace_chunks failed");

        // Force an immediate flush through the dyn handle, ahead of the periodic worker.
        let flushed = exporter.flush_client_side_stats();
        // Snapshot requests before shutdown: `Worker::shutdown` also force-flushes the stats
        // exporter, so a metrics POST captured at this point is the only proof the explicit
        // flush drove the export path. The `flushed` return value alone cannot prove this:
        // it stays true even when the weak handle fails to upgrade.
        let before_shutdown = mock_clone.captured_requests();
        exporter.shutdown(None).expect("shutdown failed");
        (flushed, before_shutdown)
    })
    .await
    .expect("spawn_blocking panicked");

    assert!(
        flushed,
        "flush_client_side_stats must report true when stats are enabled"
    );

    // The metrics request must already exist before shutdown: proves the explicit flush (not
    // the shutdown flush) sent it, and that the flush was not a no-op.
    let metrics_before: Vec<_> = before_shutdown
        .iter()
        .filter(|r| r.uri.path() == "/v1/metrics")
        .collect();
    assert_eq!(
        metrics_before.len(),
        1,
        "the explicit flush must have sent exactly one metrics request; got: {:#?}",
        before_shutdown
            .iter()
            .map(|r| format!("{} {}", r.method, r.uri))
            .collect::<Vec<_>>()
    );

    let body_before = expect_metrics_request(&before_shutdown);
    let point = single_data_point(&body_before);
    assert_eq!(point["count"], "1", "the flushed span must be counted");

    // After shutdown: the metrics request must still be present and valid.
    let body_after = expect_metrics_request(&mock.captured_requests());
    let point = single_data_point(&body_after);
    assert_eq!(point["count"], "1", "the flushed span must be counted");
}
