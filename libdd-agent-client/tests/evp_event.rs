// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use bytes::Bytes;
use httpmock::prelude::*;
use libdd_agent_client::EvpEventRequest;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn posts_to_path_with_subdomain_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/exposures")
            .header("X-Datadog-EVP-Subdomain", "event-platform-intake");
        then.status(200).body("");
    });

    common::client_for(&server)
        .send_evp_event(EvpEventRequest {
            subdomain: "event-platform-intake".to_owned(),
            path: "/api/v2/exposures".to_owned(),
            body: Bytes::from_static(b"{}"),
            content_type: "application/json".to_owned(),
        })
        .await
        .unwrap();

    mock.assert();
}
