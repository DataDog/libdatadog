// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tests for `HttpClient::send_blocking`.

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest};
use libdd_shared_runtime::SharedRuntime;
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(miri, ignore)]
#[test]
fn test_send_blocking_success() {
    ensure_crypto_provider();
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/v0.4/traces");
        then.status(200).body("ok");
    });

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Post, server.url("/v0.4/traces"))
        .with_header("Content-Type", "application/msgpack")
        .with_body(bytes::Bytes::from_static(b"test payload"));

    let response = client.send_blocking(req, &runtime).expect("request failed");

    assert_eq!(response.status_code(), 200);
    assert_eq!(response.body().as_ref(), b"ok");
    mock.assert();
}

#[cfg_attr(miri, ignore)]
#[test]
fn test_send_blocking_error_response() {
    ensure_crypto_provider();
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/error");
        then.status(503).body("service unavailable");
    });

    // Default builder has `treat_http_errors_as_errors = true`.
    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/error"));
    let result = client.send_blocking(req, &runtime);

    match result {
        Err(HttpClientError::RequestFailed { status, body }) => {
            assert_eq!(status, 503);
            assert_eq!(body, "service unavailable");
        }
        other => panic!("expected RequestFailed, got: {other:?}"),
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn test_send_blocking_timeout() {
    ensure_crypto_provider();
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/slow");
        then.status(200).delay(Duration::from_secs(10));
    });

    let client = HttpClient::new(server.url("/"), Duration::from_millis(200)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/slow"));
    let result = client.send_blocking(req, &runtime);

    assert!(
        matches!(result, Err(HttpClientError::TimedOut)),
        "expected TimedOut, got: {result:?}"
    );
}

#[cfg_attr(miri, ignore)]
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_send_blocking_works_with_uninitialised_runtime_fallback() {
    // After `before_fork`, the SharedRuntime's inner runtime is `None`.
    // `block_on` should fall back to a temporary single-threaded runtime so
    // `send_blocking` still works (per the rustdoc contract).
    ensure_crypto_provider();
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    runtime.before_fork();

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/ping");
        then.status(200).body("pong");
    });

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();
    let req = HttpRequest::new(HttpMethod::Get, server.url("/ping"));

    let response = client
        .send_blocking(req, &runtime)
        .expect("fallback runtime path should succeed");

    assert_eq!(response.status_code(), 200);
    assert_eq!(response.body().as_ref(), b"pong");
    mock.assert();
}
