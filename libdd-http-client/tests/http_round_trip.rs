// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_post_round_trip() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200).body("ok");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let mut req = HttpRequest::new(HttpMethod::Post, server.url("/v0.4/traces"));
    req.headers
        .push(("Content-Type".to_owned(), "application/msgpack".to_owned()));
    req.body = bytes::Bytes::from_static(b"test payload");

    let response = client.send(req).await.unwrap();

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body.as_ref(), b"ok");

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_get_round_trip() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"1.0"}"#);
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/info"));
    let response = client.send(req).await.unwrap();

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body.as_ref(), br#"{"version":"1.0"}"#);

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_response_headers_returned() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/headers");
            then.status(200).header("x-custom", "test-value").body("ok");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/headers"));
    let response = client.send(req).await.unwrap();

    let custom_header = response.headers.iter().find(|(name, _)| name == "x-custom");
    assert!(
        custom_header.is_some(),
        "expected x-custom header in response"
    );
    assert_eq!(custom_header.unwrap().1, "test-value");

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_4xx_returns_request_failed() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/not-found");
            then.status(404).body("not found");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/not-found"));
    let result = client.send(req).await;

    match result {
        Err(HttpClientError::RequestFailed { status, body }) => {
            assert_eq!(status, 404);
            assert_eq!(body, "not found");
        }
        other => panic!("expected RequestFailed, got: {other:?}"),
    }
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_4xx_returns_ok_when_errors_disabled() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/not-found");
            then.status(404).body("not found");
        })
        .await;

    let client = HttpClient::builder()
        .base_url(server.url("/"))
        .timeout(Duration::from_secs(5))
        .treat_http_errors_as_errors(false)
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/not-found"));
    let response = client.send(req).await.unwrap();

    assert_eq!(response.status_code, 404);
    assert_eq!(response.body.as_ref(), b"not found");
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_5xx_returns_request_failed() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/error");
            then.status(503).body("service unavailable");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Get, server.url("/error"));
    let result = client.send(req).await;

    match result {
        Err(HttpClientError::RequestFailed { status, body }) => {
            assert_eq!(status, 503);
            assert_eq!(body, "service unavailable");
        }
        other => panic!("expected RequestFailed, got: {other:?}"),
    }
}
