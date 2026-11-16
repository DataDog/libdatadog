// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{HttpClientConfig, HttpClientError, HttpRequest, HttpResponse, config};

#[cfg(all(feature = "hyper-backend", not(feature = "reqwest-backend")))]
pub(crate) mod hyper_backend;
#[cfg(feature = "reqwest-backend")]
pub(crate) mod reqwest_backend;

/// The error for a request URL whose scheme differs from the client's base URL.
///
/// A client only sends over the scheme it was configured with, so a mistake in the URL cannot
/// silently downgrade a request to plaintext or send one over a TLS setup it was not built for.
pub(crate) fn scheme_mismatch(request_url: &str) -> HttpClientError {
    HttpClientError::InvalidConfig(format!(
        "request URL '{request_url}' does not use the same scheme as the client's base URL: \
         requests are only accepted over the scheme the client was configured with"
    ))
}

/// The internal async transport backend.
///
/// This trait uses native AFIT (stable since Rust 1.75, MSRV is 1.84.1).
/// It is intentionally not object-safe — [`crate::HttpClient`] holds a concrete backend type, never
/// a `dyn Backend`.
pub(crate) trait Backend: Sized {
    /// Construct a new backend with the given timeout and transport.
    fn new(
        client_config: &HttpClientConfig,
        transport: config::TransportConfig,
    ) -> Result<Self, HttpClientError>;

    /// Send an HTTP request and return the response.
    async fn send(
        &self,
        request: HttpRequest,
        config: &config::HttpClientConfig,
    ) -> Result<HttpResponse, HttpClientError>;
}
