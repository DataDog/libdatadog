// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

use tokio::runtime;

use datadog_ipc::example_interface::{ExampleInterfaceChannel, ExampleServer};
use datadog_ipc::platform::{FileBackedHandle, ShmHandle};
use datadog_ipc::SeqpacketConn;

/// Verifies that a `ShmHandle` (Windows named file mapping) can be transferred across an IPC
/// connection via `DuplicateHandle`-based in-band handle passing, and that the receiving side
/// can successfully map the memory and read the data written by the sender.
#[test]
fn test_shm_handle_transfer() {
    let (conn_server, conn_client) = SeqpacketConn::socketpair().unwrap();

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

    // Allocate shared memory and write a known pattern into it.
    let shm = ShmHandle::new(4096).unwrap();
    let mut mapped = shm.clone().map().unwrap();
    let payload: Vec<u8> = (0u8..32).collect();
    mapped.as_slice_mut()[..32].copy_from_slice(&payload);

    // Transfer the ShmHandle to the server via IPC and ask it to sum the first 32 bytes.
    let expected_sum: u64 = payload.iter().map(|&b| b as u64).sum();
    let received_sum = channel.call_shm_sum(shm, 32).unwrap();

    assert_ne!(received_sum, u64::MAX, "shm mapping failed on server side");
    assert_eq!(received_sum, expected_sum);
}

/// Verifies that IPC messages larger than 4 KB are handled without panicking.
///
/// Before the fix, Tokio's `NamedPipeServer` registered the pipe handle with mio/IOCP, which
/// posted overlapped `ReadFile` calls into a fixed 4 KB internal buffer.  Messages larger than
/// 4 KB caused `ReadFile` to return `ERROR_MORE_DATA` synchronously; Windows still queued an
/// IOCP completion, but mio had already transitioned `io.read` to `State::Err`.  When the
/// completion fired, mio's `read_done` hit `_ => unreachable!()` (named_pipe.rs:871).
///
/// The fix routes serve-loop I/O through `block_in_place` + direct `ReadFile` into the
/// caller-supplied large buffer, bypassing mio's 4 KB limit entirely.
#[test]
fn test_large_message() {
    let (conn_server, conn_client) = SeqpacketConn::socketpair().unwrap();

    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.spawn({
        let server = ExampleServer::default();
        async move { server.accept_connection(conn_server).await }
    });

    let mut channel = ExampleInterfaceChannel::new(conn_client);

    // Send a 64 KB payload — well above mio's 4 KB internal read-buffer limit.
    let payload: Vec<u8> = (0u8..=255).cycle().take(64 * 1024).collect();
    let expected_len = payload.len() as u32;
    let received_len = channel.call_echo_len(payload).unwrap();
    assert_eq!(received_len, expected_len);
}
