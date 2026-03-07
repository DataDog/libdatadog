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

pub mod codec;
pub mod client;

pub use platform::{
    PeerCredentials, SeqpacketConn, SeqpacketListener, HANDLE_SUFFIX_SIZE, MAX_MESSAGE_SIZE,
};
#[cfg(unix)]
pub use platform::{recv_raw_async, send_raw_async};
pub use client::IpcClientConn;
