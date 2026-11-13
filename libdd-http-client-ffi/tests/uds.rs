// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

mod common;

use common::*;
use libdd_http_client_ffi::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::thread::{self, JoinHandle};

/// Answers one request on `path` and returns what the client sent up to the end of its headers.
fn serve_once(path: &Path) -> JoinHandle<String> {
    let listener = UnixListener::bind(path).expect("bind");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut head = Vec::new();
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).expect("read");
            assert!(
                read > 0,
                "connection closed before the request was complete"
            );
            head.extend_from_slice(&chunk[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .expect("write");
        String::from_utf8_lossy(&head).to_ascii_lowercase()
    })
}

#[test]
#[cfg_attr(miri, ignore)]
fn requests_go_through_the_unix_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("agent.sock");
    let server = serve_once(&socket);
    let client = Cfg::new("http://localhost")
        .unix_socket(&socket)
        .build()
        .expect("client");

    let response = client
        .send(&Req::get("/info").header("X-Test", "uds"))
        .expect("send");

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
    let head = server.join().expect("server thread");
    assert!(head.starts_with("get /info http/1.1"), "{head}");
    assert!(head.contains("x-test: uds"), "{head}");
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_host_of_the_base_url_is_ignored_with_a_unix_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("agent.sock");
    let server = serve_once(&socket);
    let client = Cfg::new("http://this-host-does-not-exist.invalid:1234")
        .unix_socket(&socket)
        .build()
        .expect("client");

    let response = client.send(&Req::get("/info")).expect("send");

    assert_eq!(response.status(), 200);
    assert!(
        server
            .join()
            .expect("server thread")
            .starts_with("get /info")
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_missing_socket_is_connection_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = Cfg::new("http://localhost")
        .unix_socket(&dir.path().join("absent.sock"))
        .build()
        .expect("client");

    let error = client.send(&Req::get("/info")).err().expect("must fail");

    assert_eq!(
        error.code,
        HttpClientErrorCode::ConnectionFailed,
        "{}",
        error.message
    );
}
