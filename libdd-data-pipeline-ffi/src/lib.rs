// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod error;
mod response;
mod trace_exporter;

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
macro_rules! catch_panic {
    ($f:expr, $err:expr) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $f)) {
            Ok(ret) => ret,
            Err(info) => {
                if let Some(s) = info.downcast_ref::<String>() {
                    tracing::error!(error = %ErrorCode::Panic, s);
                } else if let Some(s) = info.downcast_ref::<&str>() {
                    tracing::error!(error = %ErrorCode::Panic, s);
                } else {
                    tracing::error!(error = %ErrorCode::Panic, "Unable to retrieve panic context");
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
        Some(Box::new(ExporterError::new($l, &$l.to_string())))
    };
}

pub(crate) use catch_panic;
pub(crate) use gen_error;
