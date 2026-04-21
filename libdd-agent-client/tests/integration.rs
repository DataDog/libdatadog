// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests using a mock HTTP server.

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::{
    AgentClient, LanguageMetadata, TelemetryRequest, TraceFormat, TraceSendOptions,
};

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn client_for(server: &MockServer) -> AgentClient {
    ensure_crypto_provider();
    AgentClient::builder()
        .http("localhost", server.port())
        .language_metadata(LanguageMetadata::new(
            "python", "3.12.1", "CPython", "2.18.0",
        ))
        .build()
        .expect("client build failed")
}

// ── send_traces ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_traces_v5_puts_to_correct_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200).body(r#"{"rate_by_service":{}}"#);
    });

    let client = client_for(&server);
    let resp = client
        .send_traces(
            Bytes::from_static(b"\x91\x90"),
            1,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
    assert_eq!(resp.status, 200);
}

#[tokio::test]
async fn send_traces_v4_puts_to_v4_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.4/traces");
        then.status(200).body(r#"{}"#);
    });

    let client = client_for(&server);
    client
        .send_traces(
            Bytes::from_static(b"\x91\x90"),
            1,
            TraceFormat::MsgpackV4,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_traces_injects_trace_count_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("X-Datadog-Trace-Count", "42");
        then.status(200).body(r#"{}"#);
    });

    let client = client_for(&server);
    client
        .send_traces(
            Bytes::from_static(b"\x91\x90"),
            42,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_traces_injects_send_real_http_status_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("Datadog-Send-Real-Http-Status", "true");
        then.status(200).body(r#"{}"#);
    });

    let client = client_for(&server);
    client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_traces_computed_top_level_injects_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("Datadog-Client-Computed-Top-Level", "yes");
        then.status(200).body(r#"{}"#);
    });

    let client = client_for(&server);
    client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions {
                computed_top_level: true,
            },
        )
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_traces_parses_rate_by_service() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200)
            .body(r#"{"rate_by_service":{"service:env":0.75}}"#);
    });

    let client = client_for(&server);
    let resp = client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.rate_by_service
            .as_ref()
            .and_then(|m| m.get("service:env")),
        Some(&0.75)
    );
}

#[tokio::test]
async fn send_traces_returns_http_error_on_5xx() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(503).body("overloaded");
    });

    let client = client_for(&server);
    let err = client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        libdd_agent_client::SendError::HttpError { status: 503, .. }
    ));
}

// ── send_stats ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_stats_puts_to_v06_stats() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.6/stats");
        then.status(200).body("");
    });

    let client = client_for(&server);
    client
        .send_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_stats_sets_msgpack_content_type() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.6/stats")
            .header("Content-Type", "application/msgpack");
        then.status(200).body("");
    });

    let client = client_for(&server);
    client
        .send_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

// ── send_pipeline_stats ────────────────────────────────────────────────────────

#[tokio::test]
async fn send_pipeline_stats_puts_to_correct_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.1/pipeline_stats");
        then.status(200).body("");
    });

    let client = client_for(&server);
    client
        .send_pipeline_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_pipeline_stats_sets_gzip_encoding() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.1/pipeline_stats")
            .header("Content-Encoding", "gzip");
        then.status(200).body("");
    });

    let client = client_for(&server);
    client
        .send_pipeline_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

// ── send_telemetry ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_telemetry_posts_to_telemetry_proxy() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/telemetry/proxy/api/v2/apmtelemetry");
        then.status(202).body("");
    });

    let client = client_for(&server);
    client
        .send_telemetry(TelemetryRequest {
            request_type: "app-started".to_string(),
            api_version: "v2".to_string(),
            debug: false,
            body: Bytes::from_static(b"{}"),
        })
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn send_telemetry_injects_per_request_headers() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/telemetry/proxy/api/v2/apmtelemetry")
            .header("DD-Telemetry-Request-Type", "app-started")
            .header("DD-Telemetry-API-Version", "v2")
            .header("DD-Telemetry-Debug-Enabled", "false");
        then.status(202).body("");
    });

    let client = client_for(&server);
    client
        .send_telemetry(TelemetryRequest {
            request_type: "app-started".to_string(),
            api_version: "v2".to_string(),
            debug: false,
            body: Bytes::from_static(b"{}"),
        })
        .await
        .unwrap();

    mock.assert();
}

// ── send_evp_event ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_evp_event_posts_to_path_with_subdomain_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/exposures")
            .header("X-Datadog-EVP-Subdomain", "event-platform-intake");
        then.status(200).body("");
    });

    let client = client_for(&server);
    client
        .send_evp_event(
            "event-platform-intake",
            "/api/v2/exposures",
            Bytes::from_static(b"{}"),
            "application/json",
        )
        .await
        .unwrap();

    mock.assert();
}

// ── agent_info ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_info_parses_info_response() {
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

    let client = client_for(&server);
    let info = client.agent_info().await.unwrap().expect("expected Some");

    assert_eq!(info.version.as_deref(), Some("7.50.0"));
    assert!(info.endpoints.contains(&"/v0.5/traces".to_string()));
    assert!(info.client_drop_p0s);
}

#[tokio::test]
async fn agent_info_returns_none_on_404() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(404).body("not found");
    });

    let client = client_for(&server);
    let result = client.agent_info().await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn agent_info_extracts_container_tags_hash_header() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(200)
            .header("Datadog-Container-Tags-Hash", "abc123")
            .body(r#"{"endpoints":[],"client_drop_p0s":false}"#);
    });

    let client = client_for(&server);
    let info = client.agent_info().await.unwrap().unwrap();
    assert_eq!(info.container_tags_hash.as_deref(), Some("abc123"));
}

// ── static headers ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn language_metadata_headers_injected_on_all_requests() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("Datadog-Meta-Lang", "python")
            .header("Datadog-Meta-Lang-Version", "3.12.1")
            .header("Datadog-Meta-Lang-Interpreter", "CPython")
            .header("Datadog-Meta-Tracer-Version", "2.18.0")
            .header("User-Agent", "dd-trace-python/2.18.0");
        then.status(200).body(r#"{}"#);
    });

    let client = client_for(&server);
    client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn test_token_injected_when_set() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("x-datadog-test-session-token", "my-token");
        then.status(200).body(r#"{}"#);
    });

    ensure_crypto_provider();
    let client = AgentClient::builder()
        .http("localhost", server.port())
        .language_metadata(LanguageMetadata::new(
            "python", "3.12.1", "CPython", "2.18.0",
        ))
        .test_agent_session_token("my-token")
        .build()
        .unwrap();

    client
        .send_traces(
            Bytes::from_static(b""),
            0,
            TraceFormat::MsgpackV5,
            TraceSendOptions::default(),
        )
        .await
        .unwrap();

    mock.assert();
}
