// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest, MultipartPart};
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multipart_upload() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/upload");
            then.status(200).body("ok");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Post, server.url("/upload"))
        .with_multipart_part(
            MultipartPart::new("metadata", br#"{"case_id":"123"}"#.as_slice())
                .with_content_type("application/json"),
        )
        .with_multipart_part(
            MultipartPart::new("file", b"binary data here".as_slice())
                .with_filename("data.bin")
                .with_content_type("application/octet-stream"),
        );

    let response = client.send(req).await.unwrap();
    assert_eq!(response.status_code(), 200);

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multipart_sets_content_type() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/upload")
                .header_exists("content-type");
            then.status(200).body("ok");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let req = HttpRequest::new(HttpMethod::Post, server.url("/upload"))
        .with_multipart_part(MultipartPart::new("field", b"value".as_slice()));

    let response = client.send(req).await.unwrap();
    assert_eq!(response.status_code(), 200);

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multipart_replaces_an_existing_content_type() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/upload")
                .header_prefix("content-type", "multipart/form-data; boundary=")
                .header_count("^content-type$", ".*", 1);
            then.status(200);
        })
        .await;
    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    for name in ["Content-Type", "content-type"] {
        let req = HttpRequest::new(HttpMethod::Post, server.url("/upload"))
            .with_header(name, "application/json")
            .with_multipart_part(MultipartPart::new("p", b"v".as_slice()));
        client.send(req).await.unwrap();
    }

    mock.assert_calls_async(2).await;
}
