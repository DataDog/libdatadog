// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::TelemetryRequest;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn posts_to_telemetry_proxy() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/telemetry/proxy/api/v2/apmtelemetry");
        then.status(202).body("");
    });

    common::client_for(&server)
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
#[cfg_attr(miri, ignore)]
async fn injects_per_request_headers() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/telemetry/proxy/api/v2/apmtelemetry")
            .header("DD-Telemetry-Request-Type", "app-started")
            .header("DD-Telemetry-API-Version", "v2")
            .header("DD-Telemetry-Debug-Enabled", "false");
        then.status(202).body("");
    });

    common::client_for(&server)
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
