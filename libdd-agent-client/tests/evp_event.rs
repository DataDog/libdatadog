// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;

#[tokio::test]
async fn posts_to_path_with_subdomain_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/exposures")
            .header("X-Datadog-EVP-Subdomain", "event-platform-intake");
        then.status(200).body("");
    });

    common::client_for(&server)
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
