// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Unix Domain Socket transport tests.

#![cfg(unix)]

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_uds_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();

    // Spawn a minimal HTTP server on the socket.
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = stream.read(&mut buf).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });

    let client = HttpClient::builder()
        .base_url("http://localhost".to_owned())
        .timeout(Duration::from_secs(5))
        .unix_socket(&socket_path)
        .build()
        .unwrap();

    let req = HttpRequest::new(HttpMethod::Get, "http://localhost/ping".to_owned());
    let response = client.send(req).await.unwrap();

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body.as_ref(), b"ok");
}
