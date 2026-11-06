// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! C bindings for [`libdd_http_client`]: a blocking HTTP client meant to be embedded in other
//! languages' runtimes (Ruby, Python, ...).
//!
//! # Model
//!
//! * A client ([`HttpClient`]) is created once with [`ddog_http_client_new`] and reused. Creating
//!   it opens no connection; connections are pooled and opened lazily by the first request.
//! * [`ddog_http_client_send`] **blocks the calling thread** until a response arrives, an error
//!   occurs, the request times out, or the optional [`HttpCancellationToken`] is cancelled from
//!   another thread. It can be called concurrently from several threads on the same client.
//! * Every HTTP status is returned as a normal response, including 4xx and 5xx: inspect
//!   [`ddog_http_response_status`]. Errors are reserved for requests that did not produce a
//!   response.
//!
//! # Panics
//!
//! With the `catch_panic` feature (a default) every entry point catches unwinds and returns an
//! error with code [`HttpClientErrorCode::Panic`] instead of aborting the host process.

mod cancel;
mod client;
mod error;
mod request;
mod response;

pub use cancel::*;
pub use client::*;
pub use error::*;
pub use request::*;
pub use response::*;

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
macro_rules! catch_panic {
    ($f:expr, $err:expr) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $f)) {
            Ok(ret) => ret,
            Err(info) => {
                if let Some(s) = info.downcast_ref::<String>() {
                    tracing::error!("panic: {}", s);
                } else if let Some(s) = info.downcast_ref::<&str>() {
                    tracing::error!("panic: {}", s);
                } else {
                    tracing::error!("panic: unable to retrieve panic context");
                }
                $err
            }
        }
    };
}

#[cfg(any(not(feature = "catch_panic"), panic = "abort"))]
macro_rules! catch_panic {
    ($f:expr, $err:expr) => {
        $f
    };
}

pub(crate) use catch_panic;
