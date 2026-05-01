// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::{AgentClient, LanguageMetadata, TraceFormat, TraceSendOptions};

#[tokio::test]
#[cfg_attr(miri, ignore)]
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

    common::client_for(&server)
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
#[cfg_attr(miri, ignore)]
async fn test_token_injected_when_set() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.5/traces")
            .header("x-datadog-test-session-token", "my-token");
        then.status(200).body(r#"{}"#);
    });

    common::ensure_crypto_provider();
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
