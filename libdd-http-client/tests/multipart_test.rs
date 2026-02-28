// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest, MultipartPart};
use std::time::Duration;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multipart_upload() {
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/upload");
            then.status(200).body("ok");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();

    let mut req = HttpRequest::new(HttpMethod::Post, server.url("/upload"));
    req.add_multipart_part(
        MultipartPart::new("metadata", br#"{"case_id":"123"}"#.as_slice())
            .content_type("application/json"),
    );
    req.add_multipart_part(
        MultipartPart::new("file", b"binary data here".as_slice())
            .filename("data.bin")
            .content_type("application/octet-stream"),
    );

    let response = client.send(req).await.unwrap();
    assert_eq!(response.status_code, 200);

    mock.assert_async().await;
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_multipart_sets_content_type() {
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

    let mut req = HttpRequest::new(HttpMethod::Post, server.url("/upload"));
    req.add_multipart_part(MultipartPart::new("field", b"value".as_slice()));

    let response = client.send(req).await.unwrap();
    assert_eq!(response.status_code, 200);

    mock.assert_async().await;
}

#[test]
fn test_multipart_part_builder() {
    let part = MultipartPart::new("name", bytes::Bytes::from_static(b"data"))
        .filename("test.txt")
        .content_type("text/plain");

    assert_eq!(part.name, "name");
    assert_eq!(part.data.as_ref(), b"data");
    assert_eq!(part.filename.as_deref(), Some("test.txt"));
    assert_eq!(part.content_type.as_deref(), Some("text/plain"));
}
