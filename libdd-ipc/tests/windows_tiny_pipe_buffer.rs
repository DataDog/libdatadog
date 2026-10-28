// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

//! A pipe buffer smaller than the handle suffix. Its own test process, as it changes the
//! process-global pipe buffer size.

use std::panic::{AssertUnwindSafe, catch_unwind};

use libdd_ipc::platform::set_pipe_buffer_size;
use libdd_ipc::{HANDLE_SUFFIX_SIZE, SeqpacketConn, max_message_size};

#[test]
fn pipe_buffer_smaller_than_the_handle_suffix_does_not_underflow() {
    let size = 100;
    assert!(size < HANDLE_SUFFIX_SIZE);
    set_pipe_buffer_size(size);
    let max = catch_unwind(max_message_size);
    assert!(
        max.is_ok(),
        "max_message_size() panicked with a {size}-byte pipe buffer"
    );

    let (_server, client) = SeqpacketConn::socketpair().unwrap();
    let limit = catch_unwind(AssertUnwindSafe(|| client.max_send_size()));
    assert!(
        matches!(limit, Ok(limit) if limit < size),
        "max_send_size() = {limit:?} with a {size}-byte pipe buffer"
    );
}
