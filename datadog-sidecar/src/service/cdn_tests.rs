// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::blocking::{SidecarTransport, subscribe_cdn_configuration};
use crate::shm_remote_config::cdn::{CdnDeliveryState, CdnReader};
use libdd_ffe::cdn::{CdnConfig, CdnEndpoint};
use std::ffi::CString;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(url: &str) -> CdnConfig {
    CdnConfig {
        endpoint: CdnEndpoint::custom(url).unwrap(),
        language: "php".into(),
        version: "test".into(),
        poll_interval: Duration::from_secs(30),
        request_timeout: Duration::from_secs(5),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
async fn cdn_thread_mode_rejects_subscriptions_without_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&format!("http://{}/config", listener.local_addr().unwrap()));
    let (connection, _peer) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(false);
    let mut handler =
        ConnectionSidecarHandler::new(server, OwnedServerConn::new(connection).unwrap());
    assert!(
        handler
            .subscribe_cdn_configuration(Some(config))
            .await
            .is_none()
    );
    assert!(handler.cdn_subscription.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native IPC sockets and shared memory")]
async fn cdn_configuration_reaches_shared_memory_through_real_ipc_and_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&format!("http://{}/config", listener.local_addr().unwrap()));
    let http = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        let body = r#"{"data":{"type":"universal-flag-configuration","attributes":{"format":"SERVER","createdAt":"2026-10-14T00:00:00Z","environment":{"name":"test"},"flags":{}}}}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nETag: accepted\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let (server_conn, client) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(true);
    let server_task = tokio::spawn(server.accept_connection(server_conn));
    tokio::task::spawn_blocking(move || {
        let mut transport = SidecarTransport::from(client);
        transport
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let path = subscribe_cdn_configuration(&mut transport, Some(config.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(
            subscribe_cdn_configuration(&mut transport, Some(config))
                .unwrap()
                .unwrap(),
            path
        );
        let mut reader = CdnReader::new(&CString::new(path).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snapshot) = reader.read().unwrap() {
                assert_eq!(snapshot.state, CdnDeliveryState::Ready);
                assert_eq!(snapshot.revision, 1);
                assert!(
                    std::str::from_utf8(&snapshot.configuration)
                        .unwrap()
                        .contains("test")
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "CDN snapshot did not reach the client"
            );
            reader.wait_for_change(Duration::from_millis(10));
        }
        assert!(
            subscribe_cdn_configuration(&mut transport, None)
                .unwrap()
                .is_none()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if reader
                .read()
                .unwrap()
                .is_some_and(|snapshot| snapshot.state == CdnDeliveryState::Stopped)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "unsubscribe did not stop the fetcher"
            );
            reader.wait_for_change(Duration::from_millis(10));
        }
    })
    .await
    .unwrap();
    http.await.unwrap();
    // A raw macOS datagram socketpair has no peer-liveness pipe, unlike
    // normal sidecar connections. Stop its idle server after the explicit
    // unsubscribe and Stopped snapshot have already been asserted.
    server_task.abort();
    if let Err(error) = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("IPC server did not stop after its client closed")
    {
        assert!(error.is_cancelled(), "IPC server panicked: {error}");
    }
}
