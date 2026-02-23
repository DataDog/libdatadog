// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! `libdd-http-client` is an HTTP client library intended for use by other
//! languages via FFI. It offers a simple `send()` API over a concrete
//! `HttpClient` struct, with the underlying transport backend selected at
//! compile time via cargo features.
//!
//! # FIPS TLS
//!
//! When compiled with the `fips` feature (instead of `https`), TLS is enabled
//! via rustls without a default crypto provider. Call [`init_fips_crypto`]
//! once during startup before constructing any `HttpClient`:
//!
//! ```rust,ignore
//! libdd_http_client::init_fips_crypto()
//!     .expect("failed to install FIPS crypto provider");
//! ```
//!
//! The `fips` and `https` features should not be enabled simultaneously —
//! `https` pulls in a non-FIPS crypto provider which defeats the purpose.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), libdd_http_client::HttpClientError> {
//! use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
//! use std::time::Duration;
//!
//! let client = HttpClient::new("http://localhost:8080".to_string(), Duration::from_secs(5))?;
//! let request = HttpRequest::new(HttpMethod::Get, "http://localhost:8080/ping".to_string());
//! let response = client.send(request).await?;
//! println!("Status: {}", response.status_code);
//! # Ok(())
//! # }
//! ```

pub mod config;

pub(crate) mod backend;
mod client;
mod error;
mod request;
mod response;
/// Retry configuration for automatic request retries.
pub mod retry;

pub use client::HttpClient;
pub use config::{HttpClientBuilder, HttpClientConfig};
pub use error::HttpClientError;
pub use request::{HttpMethod, HttpRequest};
pub use response::HttpResponse;
pub use retry::RetryConfig;

/// Install the FIPS-compliant crypto provider for TLS.
///
/// Must be called once before constructing any [`HttpClient`] that will make
/// HTTPS requests. Only available when the `fips` feature is enabled.
///
/// Returns an error if a crypto provider has already been installed.
#[cfg(feature = "fips")]
pub fn init_fips_crypto() -> Result<(), HttpClientError> {
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .map_err(|_| {
            HttpClientError::InvalidConfig("FIPS crypto provider already installed".to_owned())
        })
}
