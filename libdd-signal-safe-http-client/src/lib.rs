// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(missing_docs)]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! `libdd-signal-safe-http-client` is a `no_std` facade over [`reqwless`].
//!
//! The crate keeps allocation and `std` support opt-in. The default build uses
//! caller-provided buffers and transports only. HTTP request encoding and
//! response parsing come from `reqwless`; TLS integration is selected by feature:
//!
//! - `mbedtls` exposes generic `MbedTLS` bindings for callers that wrap their own `embedded-io`
//!   transport.
//! - `esp-mbedtls` marks ESP mbedtls-backed integrations where callers provide the platform
//!   backend.
//!
//! Async-signal-safety still depends on the transport, allocator, TLS backend,
//! and platform hooks supplied by the caller.

/// HTTP client and connection types.
pub mod client {
    pub use reqwless::client::{
        HttpClient, HttpConnection, HttpRequestHandle, HttpResource, HttpResourceRequestBuilder,
    };
}

/// HTTP header helper types.
pub mod headers {
    pub use reqwless::headers::*;
}

/// Embedded I/O traits used by the client.
pub mod io {
    pub use embedded_io;
    pub use embedded_io_async;
    pub use embedded_nal_async;
}

/// HTTP request builders and body traits.
pub mod request {
    pub use reqwless::request::*;
}

/// HTTP response parsing and body reader types.
pub mod response {
    pub use reqwless::response::*;
}

/// TLS backend integration points.
pub mod tls {
    /// Generic `MbedTLS` bindings.
    #[cfg(feature = "mbedtls")]
    pub mod mbedtls {
        pub use ::mbedtls::*;
    }

    /// Marker for an ESP mbedtls-backed transport.
    #[cfg(feature = "esp-mbedtls")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EspMbedTls;
}

pub use client::{HttpClient, HttpConnection, HttpRequestHandle, HttpResource};
pub use reqwless::{Error, TryBufRead};

#[cfg(test)]
mod tests {
    use super::request::{Request, RequestBuilder};

    #[test]
    fn builds_reqwless_request_without_allocating() {
        let headers = [("content-type", "application/json")];
        let request = Request::post("/v0.4/traces")
            .host("localhost")
            .headers(&headers)
            .body(b"{}".as_slice())
            .build();

        let _ = request;
    }
}
