// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_ipc::SeqpacketConn;
use libdd_ipc::ipc_server::OwnedServerConn;
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
use std::sync::atomic::AtomicU64;

// Keep protocol fixtures in this test binary, out of the library's public API.
#[libdd_ipc_macros::service]
trait HandleReply {
    async fn notify();
    #[blocking]
    async fn ping();
    #[SerializedHandle]
    async fn echo_shm(#[SerializedHandle] handle: Option<ShmHandle>) -> Option<ShmHandle>;
}

struct Handler {
    connection: OwnedServerConn,
    received: AtomicU64,
}

impl HandleReply for Handler {
    fn recv_counter(&self) -> &AtomicU64 {
        &self.received
    }
    fn connection(&self) -> &OwnedServerConn {
        &self.connection
    }
    async fn notify(&mut self) {}
    async fn ping(&mut self) {}
    async fn echo_shm(&mut self, handle: Option<ShmHandle>) -> Option<ShmHandle> {
        handle
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires native IPC sockets and shared memory")]
fn typed_handle_reply_follows_buffered_acknowledgements() {
    let (server, client) = SeqpacketConn::socketpair().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.spawn(async move {
        #[cfg(windows)]
        let server = server.into_async_conn().unwrap();
        serve_handle_reply_connection(&mut Handler {
            connection: OwnedServerConn::new(server).unwrap(),
            received: AtomicU64::new(0),
        })
        .await;
    });
    let mut channel = HandleReplyChannel::new(client);
    channel.try_send_notify();
    channel.try_send_notify();
    let handle = ShmHandle::new(8192).unwrap();
    let mut original = handle.clone().map().unwrap();
    original.as_slice_mut()[8000] = 42;
    let returned = channel.call_echo_shm(Some(handle)).unwrap().unwrap();
    let mut mapped = returned.map().unwrap();
    assert_eq!(mapped.as_slice()[8000], 42);
    mapped.as_slice_mut()[8000] = 43;
    assert_eq!(
        original.as_slice()[8000],
        43,
        "reply must refer to the same mapping"
    );
    assert!(channel.call_echo_shm(None).unwrap().is_none());
    channel.call_ping().unwrap();
}
