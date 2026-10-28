// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

//! Messages of the largest size a configured pipe buffer allows.
//!
//! This file is its own test process, as it changes the process-global pipe buffer size before
//! creating pipes, as a sidecar spawned with `DD_TRACE_SIDECAR_BACKPRESSURE_BYTES` and its
//! clients do. Both ends of the pipe share that configuration.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null;

use libdd_ipc::platform::set_pipe_buffer_size;
use libdd_ipc::{HANDLE_SUFFIX_SIZE, SeqpacketConn, max_message_size, recv_raw_async};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventA, SetEvent, WaitForSingleObject};

/// Larger than the receiver's reusable buffer, smaller than the default pipe buffer.
const PIPE_BUFFER: usize = 256 * 1024 + HANDLE_SUFFIX_SIZE;

/// A message filling the pipe buffer exactly (`max_message_size()` payload bytes and every
/// handle slot) is received intact with all its handles.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn largest_message_is_received_with_its_handles() {
    set_pipe_buffer_size(PIPE_BUFFER);
    assert_eq!(max_message_size(), PIPE_BUFFER - HANDLE_SUFFIX_SIZE);
    let (server, client) = SeqpacketConn::socketpair().unwrap();

    let event = unsafe { CreateEventA(null(), 1, 0, null()) };
    assert_ne!(event, 0);
    let event = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };
    let max_handles = (HANDLE_SUFFIX_SIZE - 4) / 8;
    let expected: Vec<u8> = (0..max_message_size()).map(|i| (i % 251) as u8).collect();
    let sender = std::thread::spawn({
        let data = expected.clone();
        let raw = event.as_raw_handle() as usize;
        move || {
            let handles = vec![raw as RawHandle; max_handles];
            client.send_raw_blocking(data, &handles).map(|()| client)
        }
    });
    let (intact, handles) = recv_raw_async(&server, |b| b == expected.as_slice())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "receiving a {}-byte message through a {PIPE_BUFFER}-byte pipe failed: {e:?}",
                max_message_size()
            )
        });
    let _client = sender.join().unwrap().unwrap();
    assert!(intact, "message corrupted");
    assert_eq!(handles.len(), max_handles);
    // The last received handle is a duplicate of `event`.
    assert_ne!(
        unsafe { SetEvent(handles[max_handles - 1].as_raw_handle() as HANDLE) },
        0
    );
    assert_eq!(
        unsafe { WaitForSingleObject(event.as_raw_handle() as HANDLE, 0) },
        WAIT_OBJECT_0
    );
}
