// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A client only sends over the scheme of its base URL, on every backend: a request URL with the
//! other scheme is rejected before anything is sent, so credentials in its headers cannot be
//! downgraded to plaintext.

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn an_https_client_does_not_send_a_plain_http_request() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.path("/secret");
            then.status(200);
        })
        .await;
    let client = HttpClient::new("https://localhost:1".to_owned(), Duration::from_secs(5)).unwrap();
    let mut request = HttpRequest::new(HttpMethod::Get, server.url("/secret"));
    request
        .headers_mut()
        .push(("dd-api-key".to_owned(), "secret".to_owned()));

    let error = client.send(request).await.expect_err("scheme mismatch");

    assert!(
        matches!(error, HttpClientError::InvalidConfig(ref m) if m.contains("scheme")),
        "unexpected error: {error}"
    );
    mock.assert_calls_async(0).await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn a_plain_http_client_does_not_send_an_https_request() {
    let client = HttpClient::new("http://localhost:1".to_owned(), Duration::from_secs(5)).unwrap();
    let request = HttpRequest::new(HttpMethod::Get, "https://localhost:1/x".to_owned());

    let error = client.send(request).await.expect_err("scheme mismatch");

    assert!(
        matches!(error, HttpClientError::InvalidConfig(ref m) if m.contains("scheme")),
        "unexpected error: {error}"
    );
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn a_request_with_the_same_scheme_as_the_base_is_sent() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.path("/ok");
            then.status(200);
        })
        .await;
    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let response = client
        .send(HttpRequest::new(HttpMethod::Get, server.url("/ok")))
        .await
        .expect("send");

    assert_eq!(response.status_code(), 200);
    mock.assert_async().await;
}
