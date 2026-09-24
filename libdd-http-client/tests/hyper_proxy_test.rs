// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The proxy environment variables are read only once, ever, on the first
//! `HttpProxyConnector` constructed in the process (see `PROXY_MATCHER` in
//! `libdd_common::connector::proxy`) to avoid racing a concurrent `setenv`.
//! Therefore, there must be only a single test in thie file that touches
//! `HTTPS_PROXY`, and must set it before constructing its first `HttpClient`.
#![cfg(feature = "hyper-proxy")]

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A minimal HTTP CONNECT proxy stand-in: accepts one connection, verifies
/// it received a CONNECT request, replies with a successful tunnel
/// establishment, then closes. This is enough to prove the client routed
/// the request through the configured proxy rather than dialing the
/// destination directly.
async fn spawn_fake_connect_proxy() -> (std::net::SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hit = Arc::new(AtomicBool::new(false));
    let hit_clone = hit.clone();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };

        let mut buf = [0u8; 1024];
        let mut received = Vec::new();
        loop {
            let n = socket.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            received.extend_from_slice(&buf[..n]);
            if received.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        if received.starts_with(b"CONNECT ") {
            hit_clone.store(true, Ordering::SeqCst);
        }

        let _ = socket
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await;
        // Close without ever performing a TLS handshake: the client's request
        // is expected to fail past this point, which is fine — we only care
        // that it got routed through us.
    });

    (addr, hit)
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_https_request_is_routed_through_https_proxy() {
    ensure_crypto_provider();

    let (proxy_addr, proxy_hit) = spawn_fake_connect_proxy().await;
    // SAFETY: no other threads in this test process mutate the environment
    // concurrently.
    unsafe {
        std::env::set_var("HTTPS_PROXY", format!("http://{proxy_addr}"));
    }

    let client = HttpClient::new(
        "https://example.invalid/".to_owned(),
        Duration::from_secs(5),
    )
    .unwrap();
    let req = HttpRequest::new(HttpMethod::Get, "https://example.invalid/get".to_owned());

    // The fake proxy never completes a real TLS handshake, so the request
    // itself is expected to fail...
    let _ = client.send(req).await;

    unsafe {
        std::env::remove_var("HTTPS_PROXY");
    }

    // ...but it must have gone through the proxy rather than trying (and
    // failing on DNS resolution) to dial example.invalid directly.
    assert!(
        proxy_hit.load(Ordering::SeqCst),
        "request was not routed through the configured HTTPS_PROXY"
    );
}
