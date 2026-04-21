// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::{TraceFormat, TraceSendOptions};

#[tokio::test]
async fn v5_puts_to_correct_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200).body(r#"{"rate_by_service":{}}"#);
    });

    let client = common::client_for(&server);
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
async fn v4_puts_to_v4_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.4/traces");
        then.status(200).body(r#"{}"#);
    });

    let client = common::client_for(&server);
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
async fn injects_trace_count_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("X-Datadog-Trace-Count", "42");
        then.status(200).body(r#"{}"#);
    });

    let client = common::client_for(&server);
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
async fn injects_send_real_http_status_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("Datadog-Send-Real-Http-Status", "true");
        then.status(200).body(r#"{}"#);
    });

    let client = common::client_for(&server);
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
async fn computed_top_level_injects_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("Datadog-Client-Computed-Top-Level", "yes");
        then.status(200).body(r#"{}"#);
    });

    let client = common::client_for(&server);
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
async fn parses_rate_by_service() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(200)
            .body(r#"{"rate_by_service":{"service:env":0.75}}"#);
    });

    let client = common::client_for(&server);
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
async fn returns_http_error_on_5xx() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(PUT).path("/v0.5/traces");
        then.status(503).body("overloaded");
    });

    let client = common::client_for(&server);
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
