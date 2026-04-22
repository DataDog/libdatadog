// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;

#[tokio::test]
async fn puts_to_v06_stats() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.6/stats");
        then.status(200).body("");
    });

    common::client_for(&server)
        .send_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn sets_msgpack_content_type() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.6/stats")
            .header("Content-Type", "application/msgpack");
        then.status(200).body("");
    });

    common::client_for(&server)
        .send_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}
