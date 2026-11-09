// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use std::time::{Duration, Instant};

use tokio::runtime;

use libdd_ipc::SeqpacketConn;
use libdd_ipc::example_interface::{ExampleInterfaceChannel, ExampleServer};
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};

#[test]
#[cfg_attr(miri, ignore)]
fn test_blocking_client() {
    let (conn_server, conn_client) = SeqpacketConn::socketpair().unwrap();

    // Setup async server
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.spawn({
        let server = ExampleServer::default();
        async move { server.accept_connection(conn_server).await }
    });

    let mut channel = ExampleInterfaceChannel::new(conn_client);

    // Fire-and-forget ping (blocking variant that waits for ack)
    channel.call_ping().unwrap();

    // Blocking call with response
    let time = channel.call_time_now().unwrap();
    assert!(Instant::now().elapsed().saturating_sub(time) < Duration::from_millis(10));

    // req_cnt should be 2 (ping + time_now)
    assert_eq!(2, channel.call_req_cnt().unwrap());

    // Store a file via handle transfer
    let f = tempfile::tempfile().unwrap();
    channel.try_send_store_file(f.into());
}

/// Sets SO_SNDBUF without changing the process-wide configured packet limit, like a send buffer
/// capped by `net.core.wmem_max`.
fn shrink_send_buffer(conn: &SeqpacketConn) {
    let bytes: libc::c_int = 16 * 1024;
    // SAFETY: valid socket and pointer/length for a live integer.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                conn.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes).try_into().unwrap(),
            )
        },
        0
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn requests_exceeding_the_socket_buffer_arrive_via_shared_memory() {
    let (conn_server, conn_client) = SeqpacketConn::socketpair().unwrap();
    shrink_send_buffer(&conn_client);

    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.spawn({
        let server = ExampleServer::default();
        async move { server.accept_connection(conn_server).await }
    });

    let mut channel = ExampleInterfaceChannel::new(conn_client);
    let limit = channel.0.max_send_size();
    assert!(limit < libdd_ipc::max_message_size());

    let len = 4 * limit;
    assert_eq!(channel.call_echo_len(vec![7; len]).unwrap() as usize, len);

    // The handles of the request itself precede the shared memory carrying it.
    let mut mapped = ShmHandle::new(16).unwrap().map().unwrap();
    mapped.as_slice_mut()[..16].fill(3);
    let sum = channel
        .call_shm_sum_padded(mapped.into(), 16, vec![0; 4 * limit])
        .unwrap();
    assert_eq!(sum, 48);

    // The connection remains usable for ordinary requests.
    assert_eq!(channel.call_echo_len(vec![1; 10]).unwrap(), 10);
    assert!(!channel.0.is_closed());
}
