// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "std")]
pub mod azure_app_services;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub mod cc_utils;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub mod connector;
#[cfg(feature = "reqwest")]
#[cfg(feature = "http-client")]
pub mod dump_server;
#[cfg(feature = "std")]
mod endpoint;
#[cfg(feature = "std")]
pub mod entity_id;
#[cfg(feature = "std")]
mod lock_ext;
#[cfg(feature = "std")]
pub mod machine_id;
#[cfg(feature = "std")]
pub mod mutable_metadata;
#[cfg(feature = "std")]
pub mod regex_engine;
#[cfg(feature = "std")]
#[macro_use]
pub mod cstr;
#[cfg(feature = "bench-utils")]
pub mod bench_utils;
#[cfg(feature = "std")]
pub mod config;
#[cfg(feature = "std")]
pub mod error;
#[cfg(feature = "http-client")]
pub mod http_common;
#[cfg(feature = "std")]
pub mod multipart;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub mod rate_limiter;
pub mod tag;
#[cfg(all(feature = "std", any(test, feature = "test-utils")))]
pub mod test_utils;
#[cfg(all(feature = "std", not(target_arch = "wasm32"), not(target_os = "aix")))]
pub mod threading;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub mod timeout;
#[cfg(feature = "std")]
pub mod unix_utils;

/// Extension trait that extracts the value from a `Result` whose error type is uninhabited.
///
/// The signature constrains callers at compile time: the method is only available when the
/// error type is [`core::convert::Infallible`]. No panics — the compiler proves the `Err`
/// arm unreachable from the type.
///
/// # Examples
///
/// ```
/// use libdd_common::ResultInfallibleExt;
/// use std::convert::Infallible;
///
/// let result: Result<i32, Infallible> = Ok(42);
/// assert_eq!(result.unwrap_infallible(), 42);
/// ```
pub trait ResultInfallibleExt<T>: sealed::Sealed {
    fn unwrap_infallible(self) -> T;
}

impl<T> ResultInfallibleExt<T> for Result<T, core::convert::Infallible> {
    #[inline(always)]
    fn unwrap_infallible(self) -> T {
        match self {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for Result<T, core::convert::Infallible> {}
}

#[cfg(feature = "std")]
pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::{HeaderValue, header::HeaderName};

    pub const APPLICATION_MSGPACK_STR: &str = "application/msgpack";
    pub const APPLICATION_PROTOBUF_STR: &str = "application/x-protobuf";

    pub const DATADOG_CONTAINER_ID: HeaderName = HeaderName::from_static("datadog-container-id");
    pub const DATADOG_ENTITY_ID: HeaderName = HeaderName::from_static("datadog-entity-id");
    pub const DATADOG_EXTERNAL_ENV: HeaderName = HeaderName::from_static("datadog-external-env");
    pub const DATADOG_TRACE_COUNT: HeaderName = HeaderName::from_static("x-datadog-trace-count");
    /// Signal to the agent to send 429 responses when a payload is dropped
    /// If this is not set then the agent will always return a 200 regardless if the payload is
    /// dropped.
    pub const DATADOG_SEND_REAL_HTTP_STATUS: HeaderName =
        HeaderName::from_static("datadog-send-real-http-status");
    pub const DATADOG_API_KEY: HeaderName = HeaderName::from_static("dd-api-key");
    pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
    pub const APPLICATION_MSGPACK: HeaderValue = HeaderValue::from_static(APPLICATION_MSGPACK_STR);
    pub const APPLICATION_PROTOBUF: HeaderValue =
        HeaderValue::from_static(APPLICATION_PROTOBUF_STR);
    pub const X_DATADOG_TEST_SESSION_TOKEN: HeaderName =
        HeaderName::from_static("x-datadog-test-session-token");
}

#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub type HttpResponse = http_common::HttpResponse;
#[cfg(feature = "std")]
pub type HttpRequestBuilder = http::request::Builder;
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub trait Connect:
    hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static
{
}
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
impl<C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static> Connect
    for C
{
}

#[cfg(feature = "std")]
pub use endpoint::{Endpoint, decode_uri_path_in_authority, parse_uri};
#[cfg(feature = "std")]
pub use lock_ext::{MutexExt, RwLockExt};

// Used by tag! macro
#[cfg(feature = "alloc")]
pub use const_format;
