// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Parity tests for the `AgentClient::send_*_blocking` convenience methods.
//!
//! Each test drives the same mock-server scenario through the async API and
//! its blocking counterpart, asserting the two paths agree on the result.

mod common;

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::{SendError, TelemetryRequest, TraceFormat, TraceSendOptions};
use libdd_shared_runtime::SharedRuntime;

#[cfg_attr(miri, ignore)]
#[test]
fn send_traces_blocking_matches_async() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200)
            .body(r#"{"rate_by_service":{"service:env":0.25}}"#);
    });

    // Async baseline.
    let async_client = common::client_for(&server);
    let async_resp = runtime
        .block_on(async {
            async_client
                .send_traces(
                    Bytes::from_static(b"\x91\x90"),
                    1,
                    TraceFormat::MsgpackV5,
                    TraceSendOptions::default(),
                )
                .await
        })
        .expect("runtime block_on")
        .expect("async send_traces");

    // Blocking variant.
    let blocking_client = common::client_for(&server);
    let blocking_resp = blocking_client
        .send_traces_blocking(
            Bytes::from_static(b"\x91\x90"),
            1,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
            &runtime,
        )
        .expect("blocking send_traces");

    assert_eq!(blocking_resp.status, async_resp.status);
    assert_eq!(blocking_resp.rate_by_service, async_resp.rate_by_service);
    mock.assert_calls(2);
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_traces_blocking_propagates_http_error() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(503).body("overloaded");
    });

    let client = common::client_for(&server);
    let err = client
        .send_traces_blocking(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
            &runtime,
        )
        .unwrap_err();

    assert!(
        matches!(err, SendError::HttpError { status: 503, .. }),
        "expected HttpError 503, got: {err:?}"
    );
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_stats_blocking_matches_async() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.6/stats");
        then.status(200).body("");
    });

    let async_client = common::client_for(&server);
    runtime
        .block_on(async {
            async_client
                .send_stats(Bytes::from_static(b"\x80"))
                .await
        })
        .expect("runtime block_on")
        .expect("async send_stats");

    let blocking_client = common::client_for(&server);
    blocking_client
        .send_stats_blocking(Bytes::from_static(b"\x80"), &runtime)
        .expect("blocking send_stats");

    mock.assert_calls(2);
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_pipeline_stats_blocking_matches_async() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.1/pipeline_stats")
            .header("Content-Encoding", "gzip");
        then.status(200).body("");
    });

    let async_client = common::client_for(&server);
    runtime
        .block_on(async {
            async_client
                .send_pipeline_stats(Bytes::from_static(b"\x80"))
                .await
        })
        .expect("runtime block_on")
        .expect("async send_pipeline_stats");

    let blocking_client = common::client_for(&server);
    blocking_client
        .send_pipeline_stats_blocking(Bytes::from_static(b"\x80"), &runtime)
        .expect("blocking send_pipeline_stats");

    mock.assert_calls(2);
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_telemetry_blocking_matches_async() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/telemetry/proxy/api/v2/apmtelemetry")
            .header("DD-Telemetry-Request-Type", "app-started")
            .header("DD-Telemetry-API-Version", "v2")
            .header("DD-Telemetry-Debug-Enabled", "false");
        then.status(202).body("");
    });

    let make_req = || TelemetryRequest {
        request_type: "app-started".to_string(),
        api_version: "v2".to_string(),
        debug: false,
        body: Bytes::from_static(b"{}"),
    };

    let async_client = common::client_for(&server);
    runtime
        .block_on(async { async_client.send_telemetry(make_req()).await })
        .expect("runtime block_on")
        .expect("async send_telemetry");

    let blocking_client = common::client_for(&server);
    blocking_client
        .send_telemetry_blocking(make_req(), &runtime)
        .expect("blocking send_telemetry");

    mock.assert_calls(2);
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_evp_event_blocking_matches_async() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/exposures")
            .header("X-Datadog-EVP-Subdomain", "event-platform-intake");
        then.status(200).body("");
    });

    let async_client = common::client_for(&server);
    runtime
        .block_on(async {
            async_client
                .send_evp_event(
                    "event-platform-intake",
                    "/api/v2/exposures",
                    Bytes::from_static(b"{}"),
                    "application/json",
                )
                .await
        })
        .expect("runtime block_on")
        .expect("async send_evp_event");

    let blocking_client = common::client_for(&server);
    blocking_client
        .send_evp_event_blocking(
            "event-platform-intake",
            "/api/v2/exposures",
            Bytes::from_static(b"{}"),
            "application/json",
            &runtime,
        )
        .expect("blocking send_evp_event");

    mock.assert_calls(2);
}

#[cfg_attr(miri, ignore)]
#[test]
fn agent_info_blocking_matches_async_when_present() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(200).body(
            r#"{
                "version": "7.50.0",
                "endpoints": ["/v0.4/traces", "/v0.5/traces"],
                "client_drop_p0s": true,
                "config": {}
            }"#,
        );
    });

    let async_client = common::client_for(&server);
    let async_info = runtime
        .block_on(async { async_client.agent_info().await })
        .expect("runtime block_on")
        .expect("async agent_info")
        .expect("expected Some");

    let blocking_client = common::client_for(&server);
    let blocking_info = blocking_client
        .agent_info_blocking(&runtime)
        .expect("blocking agent_info")
        .expect("expected Some");

    assert_eq!(blocking_info.version, async_info.version);
    assert_eq!(blocking_info.endpoints, async_info.endpoints);
    assert_eq!(blocking_info.client_drop_p0s, async_info.client_drop_p0s);
}

#[cfg_attr(miri, ignore)]
#[test]
fn agent_info_blocking_returns_none_on_404() {
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(404).body("not found");
    });

    let client = common::client_for(&server);
    let result = client
        .agent_info_blocking(&runtime)
        .expect("blocking agent_info");
    assert!(result.is_none());
}

#[cfg_attr(miri, ignore)]
#[test]
fn send_traces_blocking_works_with_uninitialised_runtime_fallback() {
    // After `before_fork`, the SharedRuntime's inner runtime is `None`.
    // `block_on` should fall back to a temporary single-threaded runtime so
    // the blocking convenience methods still work (per the rustdoc contract).
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    runtime.before_fork();

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200).body(r#"{}"#);
    });

    let client = common::client_for(&server);
    let resp = client
        .send_traces_blocking(
            Bytes::from_static(b"\x91\x90"),
            1,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
            &runtime,
        )
        .expect("fallback runtime path should succeed");

    assert_eq!(resp.status, 200);
    mock.assert();
}
