// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(missing_docs)]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! Allocation-free HTTP primitives for caller-provided transports and buffers.
//!
//! Request encoding and response parsing are provided by [`reqwless`]. This
//! crate does not allocate, start a runtime, open sockets, or perform name
//! resolution itself. Async-signal-safety therefore also depends on the
//! transport, resolver, executor, and platform hooks supplied by the caller.

/// Embedded I/O traits used to provide transports and name resolution.
pub mod io {
    pub use embedded_io;
    pub use embedded_io_async;
    pub use embedded_nal_async;
}

pub use reqwless::{client, headers, request, response, Error, TryBufRead};

#[cfg(test)]
mod tests {
    use super::request::{Request, RequestBuilder};

    #[test]
    fn builds_request_without_allocating() {
        let headers = [("content-type", "application/json")];
        let request = Request::post("/telemetry/proxy/api/v2/apmtelemetry")
            .host("localhost")
            .headers(&headers)
            .body(b"{}".as_slice())
            .build();

        let _ = request;
    }
}
