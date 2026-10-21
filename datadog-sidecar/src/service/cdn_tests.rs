// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::blocking::{SidecarTransport, subscribe_cdn_configuration};
use crate::shm_remote_config::{
    RemoteConfigDeliveryState, RemoteConfigManager, RemoteConfigUpdate,
};
use libdd_ffe::cdn::{CdnConfig, CdnEndpoint};
use libdd_ffe::rules_based::UniversalFlagConfig;
use libdd_remote_config::{default_registry, fetch::ConfigInvariants};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn manager(handle: crate::shm_remote_config::RemoteConfigHandle) -> RemoteConfigManager {
    let mut manager = RemoteConfigManager::new_with_registry(
        ConfigInvariants {
            language: "php".into(),
            tracer_version: "test".into(),
            endpoint: libdd_common::Endpoint::from_slice("http://unused:8126"),
            agentless: None,
        },
        Arc::new(default_registry().with::<UniversalFlagConfig>().unwrap()),
    );
    manager.set_local_configuration(Some(handle)).unwrap();
    manager
}
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
        let handle = subscribe_cdn_configuration(&mut transport, Some(config.clone()))
            .unwrap()
            .unwrap();
        let repeated = subscribe_cdn_configuration(&mut transport, Some(config))
            .unwrap()
            .unwrap();
        let mut repeated_reader = manager(repeated);
        let mut reader = manager(handle);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let RemoteConfigUpdate::Add {
                value,
                limiter_index,
            } = reader.fetch_update()
            {
                assert_eq!(value.path.as_str(), "datadog/0/FFE_FLAGS/cdn/config");
                assert_eq!(limiter_index, 0);
                let parsed = value.data.unwrap();
                assert!(parsed.downcast::<UniversalFlagConfig>().is_some());
                assert_eq!(
                    reader.local_configuration_state(),
                    Some(RemoteConfigDeliveryState::Ready)
                );
                assert!(matches!(
                    repeated_reader.fetch_update(),
                    RemoteConfigUpdate::Add { .. }
                ));
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
            let _ = reader.fetch_update();
            if reader.local_configuration_state() == Some(RemoteConfigDeliveryState::Stopped) {
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
