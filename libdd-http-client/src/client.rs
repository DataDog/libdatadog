// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The public `HttpClient` struct.

use crate::config::{HttpClientBuilder, HttpClientConfig, TransportConfig};
use crate::{HttpClientError, HttpRequest, HttpResponse};
use std::time::Duration;

#[cfg(feature = "reqwest-backend")]
use crate::backend::Backend;

#[cfg(feature = "reqwest-backend")]
use crate::backend::reqwest_backend::ReqwestBackend;

/// A high-level async HTTP client.
///
/// Constructed once and reused across many [`HttpClient::send`] calls. Holds
/// a connection pool internally.
#[derive(Debug)]
pub struct HttpClient {
    #[cfg(feature = "reqwest-backend")]
    backend: ReqwestBackend,
    config: HttpClientConfig,
}

impl HttpClient {
    /// Construct a client for the given base URL and default timeout.
    ///
    /// This is the simple constructor for the common case. Use
    /// [`HttpClient::builder`] for advanced configuration.
    pub fn new(base_url: String, timeout: Duration) -> Result<Self, HttpClientError> {
        Self::from_config(HttpClientConfig::new(base_url, timeout))
    }

    /// Returns a builder for constructing an `HttpClient` with advanced options.
    pub fn builder() -> HttpClientBuilder {
        HttpClientBuilder::new()
    }

    pub(crate) fn from_config(config: HttpClientConfig) -> Result<Self, HttpClientError> {
        Self::from_config_and_transport(config, TransportConfig::Tcp)
    }

    pub(crate) fn from_config_and_transport(
        config: HttpClientConfig,
        transport: TransportConfig,
    ) -> Result<Self, HttpClientError> {
        #[cfg(feature = "reqwest-backend")]
        {
            let backend = ReqwestBackend::new(config.timeout(), transport)?;
            Ok(Self { backend, config })
        }
        #[cfg(not(feature = "reqwest-backend"))]
        {
            let _ = (config, transport);
            Err(HttpClientError::InvalidConfig(
                "no backend feature enabled; enable the `reqwest-backend` feature".to_owned(),
            ))
        }
    }

    /// The client's configuration.
    pub fn config(&self) -> &HttpClientConfig {
        &self.config
    }

    /// Send an HTTP request and return the response.
    ///
    /// If retry is configured, all errors except
    /// [`HttpClientError::InvalidConfig`] are retried with exponential
    /// backoff.
    pub async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        match self.config.retry() {
            Some(retry) => self.send_with_retry(request, retry).await,
            None => self.send_once(request).await,
        }
    }

    async fn send_once(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        #[cfg(feature = "reqwest-backend")]
        {
            self.backend.send(request, &self.config).await
        }
        #[cfg(not(feature = "reqwest-backend"))]
        {
            let _ = request;
            Err(HttpClientError::InvalidConfig(
                "no backend feature enabled".to_owned(),
            ))
        }
    }

    async fn send_with_retry(
        &self,
        request: HttpRequest,
        retry: &crate::retry::RetryConfig,
    ) -> Result<HttpResponse, HttpClientError> {
        let mut last_err = None;

        for attempt in 0..=retry.max_retries {
            let req = request.clone();
            match self.send_once(req).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    if attempt < retry.max_retries && crate::retry::is_retryable(&err) {
                        let delay = retry.delay_for_attempt(attempt + 1);
                        tokio::time::sleep(delay).await;
                        last_err = Some(err);
                    } else {
                        return Err(err);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            HttpClientError::IoError("retry loop ended unexpectedly".to_owned())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_client() {
        let client = HttpClient::new("http://localhost:8126".to_owned(), Duration::from_secs(3));
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.config().base_url(), "http://localhost:8126");
        assert_eq!(client.config().timeout(), Duration::from_secs(3));
    }

    #[test]
    fn builder_creates_client() {
        let client = HttpClient::builder()
            .base_url("http://localhost:8126".to_owned())
            .timeout(Duration::from_secs(5))
            .build();
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn send_returns_error_when_no_server() {
        let client =
            HttpClient::new("http://localhost".to_owned(), Duration::from_secs(1)).unwrap();
        let req = crate::HttpRequest::new(
            crate::request::HttpMethod::Get,
            "http://localhost/ping".to_owned(),
        );
        let result = client.send(req).await;
        // With the real reqwest backend, connecting to localhost with no server
        // should fail with ConnectionFailed.
        assert!(result.is_err());
    }
}
