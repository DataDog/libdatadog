// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regression tests: proxy environment variables must never influence a
//! client that was not explicitly configured with a proxy. This is the
//! behavior change that routed dd-trace-py agent traffic through customer
//! proxies (ESC APMS-21059).
//!
//! The environment is mutated here, so these tests must run in a dedicated
//! process — `cargo nextest` does this per test; plain `cargo test` does not.
use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// With HTTP_PROXY pointing at an unreachable address, a default (unproxied)
/// client must still reach the mock server directly. A client honoring the
/// environment would fail connecting to the dead proxy instead.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn default_client_ignores_http_proxy_env() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;

    // SAFETY: this test runs in its own process under nextest.
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
        std::env::remove_var("http_proxy");
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/ping");
            then.status(200).body("pong");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();
    let response = client
        .send(HttpRequest::new(HttpMethod::Get, server.url("/ping")))
        .await
        .expect("request must go direct, not through HTTP_PROXY");

    assert_eq!(response.status_code(), 200);
    mock.assert_async().await;
}

/// Same as above for ALL_PROXY, which covers both schemes.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn default_client_ignores_all_proxy_env() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;

    // SAFETY: this test runs in its own process under nextest.
    unsafe {
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:1");
        std::env::remove_var("all_proxy");
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/ping");
            then.status(200).body("pong");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();
    let response = client
        .send(HttpRequest::new(HttpMethod::Get, server.url("/ping")))
        .await
        .expect("request must go direct, not through ALL_PROXY");

    assert_eq!(response.status_code(), 200);
    mock.assert_async().await;
}
