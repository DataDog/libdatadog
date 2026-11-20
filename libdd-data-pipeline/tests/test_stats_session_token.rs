// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::{
    TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::span::v04::SpanBytes;

async fn assert_stats_session_token(token: Option<&str>) {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.path("/info");
            then.status(200)
                .body(r#"{"endpoints":["/v0.6/stats"],"client_drop_p0s":true}"#);
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200).body("{}");
        })
        .await;
    let stats = server
        .mock_async(|when, then| {
            let when = when.method(POST).path("/v0.6/stats");
            if let Some(token) = token {
                when.header("X-Datadog-Test-Session-Token", token);
            } else {
                when.header_missing("X-Datadog-Test-Session-Token");
            }
            then.status(200);
        })
        .await;
    // Respond even when the header is wrong so a regression fails the assertion without retries.
    let unmatched_stats = server
        .mock_async(|when, then| {
            when.method(POST).path("/v0.6/stats");
            then.status(200);
        })
        .await;

    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&server.base_url())
        .set_input_format(TraceExporterInputFormat::V04)
        .set_output_format(TraceExporterOutputFormat::V04)
        .enable_stats(Duration::from_secs(60));
    if let Some(token) = token {
        builder.set_test_session_token(token);
    }
    let exporter = builder.build_async::<NativeCapabilities>().await.unwrap();
    exporter
        .wait_agent_info_ready(Duration::from_secs(5))
        .await
        .unwrap();

    let span = SpanBytes {
        service: "test-service".into(),
        name: "test-operation".into(),
        resource: "GET /".into(),
        trace_id: 1,
        span_id: 1,
        start: 1_000_000_000,
        duration: 5_000_000,
        ..Default::default()
    };
    let payload = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
    exporter.send_async(&payload).await.unwrap();
    assert!(exporter.flush_client_side_stats_async().await);
    tokio::task::spawn_blocking(move || exporter.shutdown(None))
        .await
        .unwrap()
        .unwrap();

    stats.assert_calls(1);
    unmatched_stats.assert_calls(0);
}

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[tokio::test]
async fn stats_requests_include_test_session_token() {
    assert_stats_session_token(Some("background-worker-session")).await;
}

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[tokio::test]
async fn stats_requests_omit_unconfigured_test_session_token() {
    assert_stats_session_token(None).await;
}
