// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use httpmock::prelude::*;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn parses_info_response() {
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

    let info = common::client_for(&server)
        .agent_info()
        .await
        .unwrap()
        .expect("expected Some");

    assert_eq!(info.version.as_deref(), Some("7.50.0"));
    assert!(info.endpoints.contains(&"/v0.5/traces".to_string()));
    assert!(info.client_drop_p0s);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn returns_none_on_404() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(404).body("not found");
    });

    let result = common::client_for(&server).agent_info().await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn extracts_container_tags_hash_header() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(200)
            .header("Datadog-Container-Tags-Hash", "abc123")
            .body(r#"{"endpoints":[],"client_drop_p0s":false}"#);
    });

    let info = common::client_for(&server)
        .agent_info()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.container_tags_hash.as_deref(), Some("abc123"));
    assert!(!info.client_drop_p0s);
}
