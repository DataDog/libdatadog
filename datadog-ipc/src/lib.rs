// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod example_interface;
pub mod handles;

pub mod platform;
pub mod rate_limiter;
pub mod shm_stats;

mod atomic_option;
pub mod client;
pub mod codec;
pub use atomic_option::AtomicOption;

pub use client::IpcClientConn;
#[cfg(target_os = "linux")]
pub use platform::send_acks_async;

/// Maximum number of 1-byte acks buffered per connection before a forced flush.
/// Must match the `MAX_BATCH` limit inside `send_acks_async`.
pub const ACK_BUFFER_SIZE: u32 = 20;
pub use platform::{
    max_message_size, AsyncConn, PeerCredentials, SeqpacketConn, SeqpacketListener,
    HANDLE_SUFFIX_SIZE,
};
pub use platform::{recv_raw_async, send_raw_async};
