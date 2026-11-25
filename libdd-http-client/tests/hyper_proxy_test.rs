// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Explicitly configured proxying in the hyper backend. Proxy configuration
//! is per client and never derived from the environment.
#![cfg(feature = "hyper-proxy")]

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest, ProxyConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider();
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
async fn https_request_is_routed_through_explicitly_configured_proxy() {
    ensure_crypto_provider();

    let (proxy_addr, proxy_hit) = spawn_fake_connect_proxy().await;

    let client = HttpClient::builder()
        .base_url("https://example.invalid/".to_owned())
        .timeout(Duration::from_secs(5))
        .proxy(ProxyConfig::all(format!("http://{proxy_addr}")))
        .build()
        .unwrap();
    let req = HttpRequest::new(HttpMethod::Get, "https://example.invalid/get".to_owned());

    // The fake proxy never completes a real TLS handshake, so the request
    // itself is expected to fail...
    let _ = client.send(req).await;

    // ...but it must have gone through the proxy rather than trying (and
    // failing on DNS resolution) to dial example.invalid directly.
    assert!(
        proxy_hit.load(Ordering::SeqCst),
        "request was not routed through the explicitly configured proxy"
    );
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn no_proxy_entries_bypass_the_explicitly_configured_proxy() {
    ensure_crypto_provider();

    let (proxy_addr, proxy_hit) = spawn_fake_connect_proxy().await;
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut socket, _)) = server.accept().await else {
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
        let _ = socket
            .write_all("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi".as_bytes())
            .await;
    });

    let client = HttpClient::builder()
        .base_url(format!("http://{server_addr}"))
        .timeout(Duration::from_secs(5))
        .proxy(
            ProxyConfig::all(format!("http://{proxy_addr}"))
                .with_no_proxy([server_addr.ip().to_string()]),
        )
        .build()
        .unwrap();
    let req = HttpRequest::new(HttpMethod::Get, format!("http://{server_addr}/get"));

    // The request succeeds against the local server and never touches the proxy.
    let response = client.send(req).await.unwrap();
    assert_eq!(response.status_code(), 200);
    assert!(
        !proxy_hit.load(Ordering::SeqCst),
        "no_proxy destination was routed through the proxy"
    );
}

#[cfg_attr(miri, ignore)]
#[test]
fn invalid_proxy_url_is_rejected_at_build_time() {
    ensure_crypto_provider();

    let client = HttpClient::builder()
        .base_url("https://example.invalid/".to_owned())
        .timeout(Duration::from_secs(5))
        .proxy(ProxyConfig::all("not a url"))
        .build();
    assert!(
        client
            .as_ref()
            .is_err_and(|e| matches!(e, libdd_http_client::HttpClientError::InvalidConfig(_))),
        "expected InvalidConfig, got {client:?}"
    );
}
