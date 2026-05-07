// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! C ABI for `libdd-http-client`.
//!
//! Exposes [`libdd_http_client::HttpClient`] (and its builder, request,
//! response, multipart, and retry types) through a stable C ABI suitable
//! for any C-compatible language. The first consumer is dd-trace-rb but
//! the API is not Ruby-specific.
//!
//! All exported symbols use the `ddog_` prefix and follow the conventions
//! established in `libdd-common-ffi`.

mod client;
mod error;
mod multipart;
mod request;
mod response;
mod retry;
mod send;

pub use client::*;
pub use error::*;
pub use multipart::*;
pub use request::*;
pub use response::*;
pub use retry::*;
pub use send::*;
