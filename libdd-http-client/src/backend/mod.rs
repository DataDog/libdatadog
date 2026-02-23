// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "reqwest-backend")]
pub(crate) mod reqwest_backend;

/// The internal async transport backend.
///
/// This trait uses native AFIT (stable since Rust 1.75, MSRV is 1.84.1).
/// It is intentionally not object-safe — `HttpClient` holds a concrete backend
/// type, never a `dyn Backend`.
pub(crate) trait Backend {
    /// Send an HTTP request and return the response.
    async fn send(
        &self,
        request: crate::HttpRequest,
        config: &crate::config::HttpClientConfig,
    ) -> Result<crate::HttpResponse, crate::HttpClientError>;
}
