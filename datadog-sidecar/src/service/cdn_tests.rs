// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::blocking::{
    CdnSubscriptionError, SidecarTransport, SubscribeCdnConfigurationError, ping,
    subscribe_cdn_configuration,
};
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
            .unwrap()
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
async fn cdn_invalid_replacement_returns_error_and_stops_previous_publication() {
    // Keep the initial HTTP request pending: subscription errors and shutdown
    // must be observable without a successful CDN response.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original = config(&format!("http://{}/config", listener.local_addr().unwrap()));
    let (server_conn, client) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(true);
    let server_task = tokio::spawn(server.accept_connection(server_conn));
    tokio::task::spawn_blocking(move || {
        let mut transport = SidecarTransport::from(client);
        transport
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handle = subscribe_cdn_configuration(&mut transport, Some(original.clone()))
            .unwrap()
            .unwrap();
        let mut previous = manager(handle);

        let secret = "subscription-private-key";
        let environment = "subscription-private-environment";
        let mut replacement = original.clone();
        replacement.endpoint = CdnEndpoint::managed("datadoghq.com", environment, secret).unwrap();
        replacement.request_timeout = Duration::ZERO;
        let error = subscribe_cdn_configuration(&mut transport, Some(replacement)).unwrap_err();
        assert!(matches!(
            &error,
            SubscribeCdnConfigurationError::Rejected(CdnSubscriptionError::InvalidRequestTimeout)
        ));
        let diagnostic = format!("{error:?}: {error}");
        assert!(!diagnostic.contains(secret));
        assert!(!diagnostic.contains(environment));
        assert!(
            !transport.is_closed(),
            "a rejection is not a transport failure"
        );
        ping(&mut transport).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let _ = previous.fetch_update();
            if previous.local_configuration_state() == Some(RemoteConfigDeliveryState::Stopped) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "invalid replacement retained the previous subscription"
            );
            previous.wait_for_change(Duration::from_millis(10));
        }

        // An error reply must consume no descriptors or poison later replies.
        let replacement = subscribe_cdn_configuration(&mut transport, Some(original))
            .unwrap()
            .unwrap();
        let mut replacement_reader = manager(replacement);
        assert!(
            subscribe_cdn_configuration(&mut transport, None)
                .unwrap()
                .is_none()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let _ = replacement_reader.fetch_update();
            if replacement_reader.local_configuration_state()
                == Some(RemoteConfigDeliveryState::Stopped)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "replacement publication did not remain usable after the error reply"
            );
            replacement_reader.wait_for_change(Duration::from_millis(10));
        }
    })
    .await
    .unwrap();
    // Raw macOS socketpairs need an explicit server stop, as in the HTTP test below.
    server_task.abort();
    if let Err(error) = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("IPC server did not stop after its client closed")
    {
        assert!(error.is_cancelled(), "IPC server panicked: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native IPC sockets and shared memory")]
async fn cdn_ipc_reader_cannot_modify_publication_or_crash_unsubscribe() {
    use libdd_ipc::handles::{FdSink, TransferHandles};
    #[cfg(target_os = "linux")]
    use std::os::fd::AsRawFd;
    use std::os::fd::RawFd;

    fn assert_mutations_rejected(fd: RawFd) {
        // SAFETY: the caller keeps the descriptor owner alive. No mapping is
        // accessed, and the byte buffer remains valid throughout pwrite.
        assert_eq!(
            unsafe { libc::ftruncate(fd, 0) },
            -1,
            "a subscriber must not truncate the publisher's live mapping"
        );
        let byte = [0xff_u8];
        assert_eq!(
            unsafe { libc::pwrite(fd, byte.as_ptr().cast(), byte.len(), 0) },
            -1,
            "a subscriber must not overwrite publication bytes"
        );
        let mapping_len = 4096;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if mapping != libc::MAP_FAILED {
            // SAFETY: this is the mapping returned above, with its original length.
            unsafe { libc::munmap(mapping, mapping_len) };
            panic!("a subscriber must not obtain a shared writable mapping");
        }
    }

    // No CDN response is needed to obtain the publication or stop it. Keep the
    // endpoint listening so the fetch remains pending while checking the handle.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&format!("http://{}/config", listener.local_addr().unwrap()));
    let (server_conn, client) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(true);
    let server_task = tokio::spawn(server.accept_connection(server_conn));
    tokio::task::spawn_blocking(move || {
        let mut transport = SidecarTransport::from(client);
        transport
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handle = subscribe_cdn_configuration(&mut transport, Some(config))
            .unwrap()
            .unwrap();
        let mut sink = FdSink::new();
        handle.copy_handles(&mut sink).unwrap();
        assert_eq!(sink.fds().len(), 1);
        let fd = sink.fds()[0];
        // SAFETY: handle owns the descriptor received through the actual IPC reply.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_ne!(flags, -1);
        // Fail before attempting truncation if an unsafe writable handle returns.
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert_mutations_rejected(fd);

        #[cfg(target_os = "linux")]
        {
            // O_RDONLY alone is insufficient on Linux: /proc permits reopening
            // a memfd for writing. Require kernel seals before probing that path.
            // SAFETY: handle still owns fd; fcntl only reads the seal set here.
            let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
            assert_ne!(seals, -1);
            let required = libc::F_SEAL_SHRINK | libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL;
            assert_eq!(seals & required, required);
            let reopened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(format!("/proc/self/fd/{fd}"))
                .unwrap();
            let reopened_fd = reopened.as_raw_fd();
            // SAFETY: reopened owns this valid descriptor.
            assert_eq!(
                unsafe { libc::fcntl(reopened_fd, libc::F_GETFL) } & libc::O_ACCMODE,
                libc::O_RDWR
            );
            assert_mutations_rejected(reopened_fd);
            // A subscriber must not stop the publisher from growing its segment.
            // SAFETY: reopened still owns the descriptor; F_SEAL_SEAL must reject this.
            assert_eq!(
                unsafe { libc::fcntl(reopened_fd, libc::F_ADD_SEALS, libc::F_SEAL_GROW) },
                -1
            );
        }

        let mut reader = manager(handle);
        assert!(
            subscribe_cdn_configuration(&mut transport, None)
                .unwrap()
                .is_none()
        );
        ping(&mut transport).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let _ = reader.fetch_update();
            if reader.local_configuration_state() == Some(RemoteConfigDeliveryState::Stopped) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the publisher must survive mutation attempts and publish Stopped"
            );
            reader.wait_for_change(Duration::from_millis(10));
        }
    })
    .await
    .unwrap();
    // Raw macOS socketpairs need an explicit server stop after the assertions.
    server_task.abort();
    if let Err(error) = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("IPC server did not stop after its client closed")
    {
        assert!(error.is_cancelled(), "IPC server panicked: {error}");
    }
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
