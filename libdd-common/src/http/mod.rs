// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared HTTP foundation used by both `libdd-http-client` backends and by
//! existing libdatadog consumers (telemetry, crashtracker, remote-config,
//! tracer-flare, …).
//!
//! ## Scope
//!
//! This module is the single source of truth for:
//!
//! - The hyper-compatible request/response [`Body`] (with channel-based
//!   streaming support).
//! - Generic [`GenericHttpClient`] / [`HttpRequest`] / [`HttpResponse`]
//!   aliases plus convenience constructors ([`new_default_client`],
//!   [`new_client_periodic`]).
//! - Error categorisation ([`Error`], [`ClientError`], [`ErrorKind`]) shared
//!   across backends and across the data-pipeline error surface.
//! - FIPS provider installation ([`install_fips_provider`], `cfg(feature = "fips")`).
//!
//! ## Backwards compatibility
//!
//! The legacy module path `libdd_common::http_common` is preserved as a
//! re-export shim. Existing consumers continue to compile unchanged; new
//! consumers are encouraged to import from `libdd_common::http` directly.

pub mod body;
pub mod client;
pub mod error;
#[cfg(feature = "fips")]
pub mod fips;

#[cfg(test)]
mod tests;

// --- Re-exports forming the public surface of `libdd_common::http` ---

pub use error::{ClientError, Error, ErrorKind};

#[cfg(not(target_arch = "wasm32"))]
pub use body::{Body, Sender};

#[cfg(not(target_arch = "wasm32"))]
pub use client::{
    client_builder, collect_response_bytes, empty_response, into_response, mock_response,
    new_client_periodic, new_default_client, GenericHttpClient, HttpRequest, HttpRequestError,
    HttpResponse, ResponseFuture,
};

#[cfg(feature = "fips")]
pub use fips::{install_fips_provider, FipsInstallError};
