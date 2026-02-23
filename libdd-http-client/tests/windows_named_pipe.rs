// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ServerOptions;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_named_pipe_round_trip() {
    let pipe_name = format!(
        r"\\.\pipe\dd_http_client_test_{}_{}",
        std::process::id(),
        fastrand::u64(..)
    );

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
        .unwrap();

    let pipe_name_clone = pipe_name.clone();
    tokio::spawn(async move {
        server.connect().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = server.read(&mut buf).await.unwrap();
        server
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });

    let client = HttpClient::builder()
        .base_url("http://localhost".to_owned())
        .timeout(Duration::from_secs(5))
        .windows_named_pipe(&pipe_name_clone)
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, "http://localhost/ping".to_owned());
    let response = client.send(req).await.unwrap();

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body.as_ref(), b"ok");
}

#[test]
fn test_named_pipe_client_constructs() {
    let client = HttpClient::builder()
        .base_url("http://localhost".to_owned())
        .timeout(Duration::from_secs(5))
        .windows_named_pipe(r"\\.\pipe\dd_test_construct")
        .build();
    assert!(client.is_ok());
}
