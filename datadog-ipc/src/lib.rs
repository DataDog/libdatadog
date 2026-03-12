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

pub mod client;
pub mod codec;

pub use client::IpcClientConn;
pub use platform::{recv_raw_async, send_raw_async};
pub use platform::{
    max_message_size, PeerCredentials, SeqpacketConn, SeqpacketListener, HANDLE_SUFFIX_SIZE,
};
