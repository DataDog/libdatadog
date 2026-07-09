// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! `no_std`-first HTTP/1.1 request emission for signal-safe Datadog telemetry submissions.
//!
//! The default build has no allocator, DNS, socket, thread, lock, TLS, or runtime dependency. It
//! only validates and writes HTTP request bytes into caller-owned sinks. That makes the crate's
//! default request-emission path suitable for use from an async signal handler when the supplied
//! sink is also signal-safe.
//!
//! Feature flags intentionally separate handler-safe and convenience APIs:
//!
//! - `alloc`: enables owned request buffers such as [`Request::to_vec`].
//! - `std`: enables standard library support and implies `alloc`.
//! - `libc-dns`: enables weakly loaded libc `getaddrinfo` helpers for setup paths.
//!
//! Transport remains caller-owned. For signal-handler use, prepare any connection, file
//! descriptor, or socket state before entering the handler and make the sink's `write_all`
//! implementation obey the platform's async-signal-safety rules.

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(test)]
extern crate std;

mod error;
mod header;
mod request;
mod sink;
mod telemetry;

#[cfg(feature = "libc-dns")]
pub mod dns;

pub use error::{BuildError, SendError};
pub use header::Header;
pub use request::{HttpClient, Request};
pub use reqwless::request::Method;
#[cfg(feature = "std")]
pub use sink::StdWriteSink;
pub use sink::{BufferTooSmall, FixedBuffer, HttpSink};
pub use telemetry::{
    TelemetryMetricsRequest, AGENT_TELEMETRY_PATH, APPLICATION_JSON, DIRECT_TELEMETRY_PATH,
    HEADER_API_VERSION, HEADER_DEBUG_ENABLED, HEADER_REQUEST_TYPE, REQUEST_TYPE_GENERATE_METRICS,
    TELEMETRY_API_VERSION_V2,
};
