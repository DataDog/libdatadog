// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multiple_requests_reuse_client() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/ping");
            then.status(200).body("pong");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    for i in 0..5 {
        let req = HttpRequest::new(HttpMethod::Get, server.url("/ping"));
        let response = client.send(req).await.unwrap();
        assert_eq!(
            response.status_code, 200,
            "request {i} should have succeeded"
        );
    }

    // Verify the mock was hit exactly 5 times, confirming all requests
    // went through the same HttpClient (and its underlying reqwest::Client
    // connection pool).
    mock.assert_calls_async(5).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_concurrent_requests_succeed() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/ping");
            then.status(200).body("pong");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req1 = HttpRequest::new(HttpMethod::Get, server.url("/ping"));
    let req2 = HttpRequest::new(HttpMethod::Get, server.url("/ping"));
    let req3 = HttpRequest::new(HttpMethod::Get, server.url("/ping"));

    let (r1, r2, r3) = tokio::join!(client.send(req1), client.send(req2), client.send(req3));

    assert_eq!(r1.unwrap().status_code, 200);
    assert_eq!(r2.unwrap().status_code, 200);
    assert_eq!(r3.unwrap().status_code, 200);

    mock.assert_calls_async(3).await;
}
