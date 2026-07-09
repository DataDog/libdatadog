// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! `no_std`-first Datadog HTTP helpers for `reqwless`.
//!
//! The default build uses `reqwless` with `default-features = false`; it does not enable an
//! allocator, DNS, sockets, threads, locks, TLS, or a runtime. Callers either use the re-exported
//! reqwless APIs directly or provide [`HttpClient`] with pre-opened synchronous I/O.
//!
//! Feature flags intentionally separate handler-safe and convenience APIs:
//!
//! - `alloc`: enables allocation-backed setup helpers.
//! - `std`: reserves standard-library support and implies `alloc`.
//! - `libc-dns`: enables weakly loaded libc `getaddrinfo` helpers for setup paths.
//!
//! Request construction is reqwless-based. This crate re-exports reqwless, provides a synchronous
//! client façade for callers that do not want async in their API, and includes Datadog telemetry
//! builders as an example payload family.

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(test)]
extern crate std;

mod client;
mod telemetry;

#[cfg(feature = "libc-dns")]
pub mod dns;

pub use client::{ClientError, HttpClient, HttpResponse};
pub use embedded_io;
pub use reqwless::{
    self,
    headers::ContentType,
    request::{Method, Request, RequestBody, RequestBuilder},
    response::{Response, Status, StatusCode},
    Error,
};
pub use telemetry::{
    agent_telemetry_metrics_request, telemetry_metrics_headers, telemetry_metrics_request, Header,
    TelemetryMetricsRequestBuilder, AGENT_TELEMETRY_PATH, APPLICATION_JSON, CONNECTION_CLOSE,
    DIRECT_TELEMETRY_PATH, HEADER_API_VERSION, HEADER_DEBUG_ENABLED, HEADER_REQUEST_TYPE,
    REQUEST_TYPE_GENERATE_METRICS, TELEMETRY_API_VERSION_V2,
};
