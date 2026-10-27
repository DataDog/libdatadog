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
    #[SerializedHandle]
    async fn echo_shm_padded(#[SerializedHandle] handle: ShmHandle, padding: Vec<u8>) -> ShmHandle;
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
    async fn echo_shm_padded(&mut self, handle: ShmHandle, padding: Vec<u8>) -> ShmHandle {
        assert!(padding.iter().all(|byte| *byte == 7));
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

#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore = "requires native IPC sockets and shared memory")]
fn oversized_request_preserves_handle_bearing_reply() {
    let (server, client) = SeqpacketConn::socketpair().unwrap();
    // Reduce the kernel send buffer without changing the process-wide message limit.
    let bytes: libc::c_int = 16 * 1024;
    // SAFETY: valid socket and pointer/length for a live integer.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                client.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes).try_into().unwrap(),
            )
        },
        0
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.spawn(async move {
        serve_handle_reply_connection(&mut Handler {
            connection: OwnedServerConn::new(server).unwrap(),
            received: AtomicU64::new(0),
        })
        .await;
    });
    let mut channel = HandleReplyChannel::new(client);
    channel
        .0
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let limit = channel.0.max_send_size();
    assert!(4 * limit < libdd_ipc::max_message_size());
    channel.try_send_notify();
    let handle = ShmHandle::new(8192).unwrap();
    let mut original = handle.clone().map().unwrap();
    original.as_slice_mut()[8000] = 42;
    let returned = channel
        .call_echo_shm_padded(handle, vec![7; 4 * limit])
        .unwrap();
    let mut mapped = returned.map().unwrap();
    assert_eq!(mapped.as_slice()[8000], 42);
    mapped.as_slice_mut()[8000] = 43;
    assert_eq!(original.as_slice()[8000], 43);
    channel.call_ping().unwrap();
    assert!(!channel.0.is_closed());
}
