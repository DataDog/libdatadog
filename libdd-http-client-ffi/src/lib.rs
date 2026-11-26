// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! C bindings for [`libdd_http_client`]: a blocking HTTP client.

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

macro_rules! gen_error {
    ($l:expr) => {
        Some(Box::new(HttpClientError::new($l, &$l.to_string())))
    };
}

pub(crate) use catch_panic;
pub(crate) use gen_error;
