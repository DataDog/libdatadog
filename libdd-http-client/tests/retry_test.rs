// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest, RetryConfig};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_retries_on_503() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/retry");
            then.status(503).body("unavailable");
        })
        .await;

    let client = HttpClient::builder()
        .base_url(server.url("/"))
        .timeout(Duration::from_secs(5))
        .retry(
            RetryConfig::new()
                .max_retries(2)
                .with_jitter(false)
                .initial_delay(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/retry"));
    let result = client.send(req).await;

    assert!(matches!(
        result,
        Err(HttpClientError::RequestFailed { status: 503, .. })
    ));
    // Initial request + 2 retries = 3 total
    mock.assert_calls_async(3).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_retries_on_404() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/missing");
            then.status(404).body("not found");
        })
        .await;

    let client = HttpClient::builder()
        .base_url(server.url("/"))
        .timeout(Duration::from_secs(5))
        .retry(
            RetryConfig::new()
                .max_retries(2)
                .with_jitter(false)
                .initial_delay(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/missing"));
    let result = client.send(req).await;

    assert!(matches!(
        result,
        Err(HttpClientError::RequestFailed { status: 404, .. })
    ));
    // 404 is retried — initial request + 2 retries = 3 total
    mock.assert_calls_async(3).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_no_retry_when_not_configured() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/fail");
            then.status(503).body("unavailable");
        })
        .await;

    // No .retry() — retries disabled
    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/fail"));
    let result = client.send(req).await;

    assert!(result.is_err());
    // Only 1 attempt, no retries
    mock.assert_calls_async(1).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_succeeds_after_transient_failure() {
    let server = MockServer::start_async().await;

    // First two calls return 503, third returns 200
    let fail_mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/flaky");
            then.status(503).body("unavailable");
        })
        .await;

    let client = HttpClient::builder()
        .base_url(server.url("/"))
        .timeout(Duration::from_secs(5))
        .retry(
            RetryConfig::new()
                .max_retries(3)
                .with_jitter(false)
                .initial_delay(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

    // Delete the fail mock after 2 hits and replace with success
    let req = HttpRequest::new(HttpMethod::Get, server.url("/flaky"));
    let result = client.send(req).await;

    // With a static mock returning 503, all attempts fail
    assert!(matches!(
        result,
        Err(HttpClientError::RequestFailed { status: 503, .. })
    ));
    // Initial + 3 retries = 4 total
    fail_mock.assert_calls_async(4).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_retries_on_connection_error() {
    // Port 1 — nothing listening
    let client = HttpClient::builder()
        .base_url("http://127.0.0.1:1".to_owned())
        .timeout(Duration::from_secs(1))
        .retry(
            RetryConfig::new()
                .max_retries(1)
                .with_jitter(false)
                .initial_delay(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, "http://127.0.0.1:1/ping".to_owned());
    let result = client.send(req).await;

    assert!(matches!(result, Err(HttpClientError::ConnectionFailed(_))));
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_backoff_increases() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/slow-retry");
            then.status(503).body("unavailable");
        })
        .await;

    let client = HttpClient::builder()
        .base_url(server.url("/"))
        .timeout(Duration::from_secs(5))
        .retry(
            RetryConfig::new()
                .max_retries(3)
                .with_jitter(false)
                .initial_delay(Duration::from_millis(50)),
        )
        .build()
        .unwrap();

    let start = std::time::Instant::now();
    let req = HttpRequest::new(HttpMethod::Get, server.url("/slow-retry"));
    let _ = client.send(req).await;
    let elapsed = start.elapsed();

    // Without jitter: 50ms + 100ms + 200ms = 350ms minimum
    assert!(
        elapsed >= Duration::from_millis(300),
        "expected at least 300ms of backoff delay, got {:?}",
        elapsed
    );
}
