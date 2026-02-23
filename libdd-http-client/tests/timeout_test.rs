// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_request_times_out() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/slow");
            then.status(200).delay(Duration::from_secs(10));
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_millis(200)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/slow"));
    let result = client.send(req).await;

    assert!(
        matches!(result, Err(HttpClientError::TimedOut)),
        "expected TimedOut, got: {result:?}"
    );
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_per_request_timeout_overrides_client() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/slow");
            then.status(200).delay(Duration::from_secs(2));
        })
        .await;

    // Client timeout is generous (5s), but per-request timeout is tight (200ms).
    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let mut req = HttpRequest::new(HttpMethod::Get, server.url("/slow"));
    req.timeout = Some(Duration::from_millis(200));

    let result = client.send(req).await;

    assert!(
        matches!(result, Err(HttpClientError::TimedOut)),
        "expected TimedOut from per-request timeout, got: {result:?}"
    );
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_connection_refused() {
    // Use port 1 which is very unlikely to have a listener.
    let client = HttpClient::new("http://127.0.0.1:1".to_owned(), Duration::from_secs(1)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, "http://127.0.0.1:1/ping".to_owned());
    let result = client.send(req).await;

    assert!(result.is_err());
}
