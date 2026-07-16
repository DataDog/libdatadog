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
//! crate does not allocate or start a runtime. The optional `rustix-tcp`
//! feature provides a blocking TCP transport without performing name
//! resolution. Async-signal-safety also depends on the resolver, executor, and
//! platform hooks supplied by the caller.

pub mod dns;
pub mod env;

#[cfg(feature = "libc_dns")]
pub mod libc_dns;

#[cfg(feature = "rustix-tcp")]
pub mod rustix;

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
    #[cfg(feature = "rustix-tcp")]
    use super::rustix;

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

    #[cfg(feature = "rustix-tcp")]
    #[test]
    fn rustix_stream_supports_sync_and_async_io() {
        fn assert_sync<T: embedded_io::Read + embedded_io::Write>() {}
        fn assert_async<T: embedded_io_async::Read + embedded_io_async::Write>() {}
        fn assert_connector<T: embedded_nal_async::TcpConnect>() {}

        assert_sync::<rustix::TcpStream>();
        assert_async::<rustix::TcpStream>();
        assert_connector::<rustix::TcpConnector>();
    }
}
