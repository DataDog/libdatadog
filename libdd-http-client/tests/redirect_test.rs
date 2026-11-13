// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn redirects_are_returned_and_not_followed() {
    ensure_crypto_provider();
    let origin = MockServer::start_async().await;
    let target = MockServer::start_async().await;
    let redirect = origin
        .mock_async(|when, then| {
            when.method(POST).path("/redirect");
            then.status(307).header("Location", target.url("/target"));
        })
        .await;
    let reached = target
        .mock_async(|when, then| {
            when.path_prefix("/");
            then.status(200);
        })
        .await;
    let client = HttpClient::new(origin.url("/"), Duration::from_secs(5)).unwrap();

    let request = HttpRequest::new(HttpMethod::Post, origin.url("/redirect"))
        .with_header("DD-API-KEY", "secret")
        .with_body("payload");
    let response = client.send(request).await.unwrap();

    assert_eq!(response.status_code(), 307);
    redirect.assert_async().await;
    // Nothing reaches the other origin, so neither the API key nor the body does.
    reached.assert_calls_async(0).await;
}

#[cfg(feature = "reqwest-backend")]
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn redirects_are_followed_when_enabled() {
    ensure_crypto_provider();
    let origin = MockServer::start_async().await;
    let target = MockServer::start_async().await;
    origin
        .mock_async(|when, then| {
            when.path("/redirect");
            then.status(307).header("Location", target.url("/target"));
        })
        .await;
    let reached = target
        .mock_async(|when, then| {
            when.path("/target");
            then.status(200).body("final");
        })
        .await;
    let client = HttpClient::builder()
        .base_url(origin.url("/"))
        .timeout(Duration::from_secs(5))
        .follow_redirects(true)
        .build()
        .unwrap();

    let request = HttpRequest::new(HttpMethod::Get, origin.url("/redirect"));
    let response = client.send(request).await.unwrap();

    assert_eq!(response.status_code(), 200);
    assert_eq!(response.body().as_ref(), b"final");
    reached.assert_async().await;
}

#[cfg(not(feature = "reqwest-backend"))]
#[cfg_attr(miri, ignore)]
#[test]
fn following_redirects_is_not_supported_by_the_hyper_backend() {
    ensure_crypto_provider();

    let result = HttpClient::builder()
        .base_url("http://localhost".to_owned())
        .timeout(Duration::from_secs(1))
        .follow_redirects(true)
        .build();

    assert!(matches!(
        result,
        Err(libdd_http_client::HttpClientError::InvalidConfig(_))
    ));
}
