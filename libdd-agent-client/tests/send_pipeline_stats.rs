// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;

#[tokio::test]
async fn puts_to_correct_endpoint() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT).path("/v0.1/pipeline_stats");
        then.status(200).body("");
    });

    let client = common::client_for(&server);
    client
        .send_pipeline_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn sets_gzip_encoding() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.1/pipeline_stats")
            .header("Content-Encoding", "gzip");
        then.status(200).body("");
    });

    let client = common::client_for(&server);
    client
        .send_pipeline_stats(Bytes::from_static(b"\x80"))
        .await
        .unwrap();

    mock.assert();
}
