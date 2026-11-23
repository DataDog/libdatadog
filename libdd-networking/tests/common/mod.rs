// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by the client integration tests.

#![allow(dead_code)]

extern crate alloc;

use alloc::sync::Arc;
use core::net::SocketAddr;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use libdd_networking::connector::Connector;
use libdd_networking::http_common::{self, Body, GenericHttpClient};

/// HTTP/1.1 keep-alive server running on plain threads, so it uses no tokio runtime.
pub struct KeepAliveServer {
    pub addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
}

impl KeepAliveServer {
    /// Start a server that answers `ok` to every request and keeps connections open.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || serve(stream));
            }
        });
        Self { addr, accepted }
    }

    /// Number of connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

fn serve(stream: std::net::TcpStream) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        // Read the request headers until the empty line. Requests in these tests have no body.
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => {}
            }
        }
        if writer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
            .is_err()
        {
            return;
        }
    }
}

/// Send a GET request and check the server answered `ok`.
pub async fn get_ok(client: &GenericHttpClient<Connector>, addr: SocketAddr) {
    let req = http::Request::get(format!("http://{addr}/"))
        .body(Body::empty())
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = http_common::collect_response_bytes(http_common::into_response(resp))
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"ok");
}
