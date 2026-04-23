// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{config, HttpClientError, HttpRequest, HttpResponse};
use std::time::Duration;

#[cfg(all(feature = "hyper-backend", not(feature = "reqwest-backend")))]
pub(crate) mod hyper_backend;
#[cfg(feature = "reqwest-backend")]
pub(crate) mod reqwest_backend;

/// The internal async transport backend.
///
/// This trait uses native AFIT (stable since Rust 1.75, MSRV is 1.84.1).
/// It is intentionally not object-safe — [HttpClient] holds a concrete backend type, never a `dyn
/// Backend`.
pub(crate) trait Backend: Sized {
    /// Construct a new backend with the given timeout and transport.
    fn new(timeout: Duration, transport: config::TransportConfig) -> Result<Self, HttpClientError>;

    /// Send an HTTP request and return the response.
    async fn send(
        &self,
        request: HttpRequest,
        config: &config::HttpClientConfig,
    ) -> Result<HttpResponse, HttpClientError>;
}
