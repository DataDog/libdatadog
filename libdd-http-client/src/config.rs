// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Configuration types for `libdd-http-client`.

use std::time::Duration;

use crate::retry::RetryConfig;

/// Transport configuration for the HTTP backend.
///
/// This is a construction-time concern — once the `reqwest::Client` is built,
/// the transport is embedded in the client and this value is not retained.
#[derive(Debug, Clone, Default)]
pub(crate) enum TransportConfig {
    /// Standard TCP transport (HTTP or HTTPS depending on URL scheme).
    #[default]
    Tcp,
    /// Unix Domain Socket transport.
    #[cfg(unix)]
    UnixSocket(std::path::PathBuf),
    /// Windows Named Pipe transport.
    #[cfg(windows)]
    WindowsNamedPipe(std::ffi::OsString),
}

/// Configuration for an [`crate::HttpClient`] instance.
///
/// Constructed via [`crate::HttpClient::new`] or [`HttpClientBuilder::build`].
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    base_url: String,
    timeout: Duration,
    treat_http_errors_as_errors: bool,
    retry: Option<RetryConfig>,
    allow_connection_pooling: bool,
}

impl HttpClientConfig {
    /// Create a config with the given base URL and timeout. HTTP errors are
    /// treated as errors by default.
    pub(crate) fn new(base_url: String, timeout: Duration) -> Self {
        Self {
            base_url,
            timeout,
            treat_http_errors_as_errors: true,
            retry: None,
            allow_connection_pooling: true,
        }
    }

    /// The base URL for this client.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The default request timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether HTTP 4xx/5xx responses are returned as errors.
    pub fn treat_http_errors_as_errors(&self) -> bool {
        self.treat_http_errors_as_errors
    }

    /// The retry configuration, if retries are enabled.
    pub fn retry(&self) -> Option<&RetryConfig> {
        self.retry.as_ref()
    }

    /// Whether connection pooling can be used, when available. See
    /// [HttpClientBuilder::allow_connection_pooling].
    pub fn allow_connection_pooling(&self) -> bool {
        self.allow_connection_pooling
    }
}

/// Builder for [`crate::HttpClient`].
///
/// Obtain via [`crate::HttpClient::builder`].
#[derive(Debug)]
pub struct HttpClientBuilder {
    base_url: Option<String>,
    timeout: Option<Duration>,
    treat_http_errors_as_errors: bool,
    retry: Option<RetryConfig>,
    transport: TransportConfig,
    allow_connection_pooling: bool,
}

impl Default for HttpClientBuilder {
    fn default() -> Self {
        Self {
            base_url: Default::default(),
            timeout: Default::default(),
            treat_http_errors_as_errors: true,
            retry: Default::default(),
            transport: Default::default(),
            allow_connection_pooling: true,
        }
    }
}

impl HttpClientBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base URL.
    pub fn base_url(mut self, url: String) -> Self {
        self.base_url = Some(url);
        self
    }

    /// Set the default request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Configure whether HTTP 4xx/5xx responses are returned as errors.
    ///
    /// Default: `true`. Set to `false` to return all responses as successful,
    /// regardless of status code.
    pub fn treat_http_errors_as_errors(mut self, value: bool) -> Self {
        self.treat_http_errors_as_errors = value;
        self
    }

    /// Enable automatic retries with the given configuration.
    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
    }

    /// Route all connections through the given Unix Domain Socket.
    ///
    /// The host portion of the URL is ignored for routing — all requests
    /// are sent over the socket regardless of the URL's host.
    #[cfg(unix)]
    pub fn unix_socket(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.transport = TransportConfig::UnixSocket(path.into());
        self
    }

    /// Route all connections through the given Windows Named Pipe.
    ///
    /// The host portion of the URL is ignored for routing — all requests
    /// are sent over the pipe regardless of the URL's host.
    #[cfg(windows)]
    pub fn windows_named_pipe(mut self, pipe: impl Into<std::ffi::OsString>) -> Self {
        self.transport = TransportConfig::WindowsNamedPipe(pipe.into());
        self
    }

    /// Allow connection pooling. Defaults to `true`.
    ///
    /// Note that whether pooling is actually used depends on the HTTP backend of
    /// [libdd_http_client], though both currently available backends (reqwest and hyper) support
    /// pooling. This setting should be understood as: if set to `true`, the default behavior of the
    /// underlying backend will be selected, which might or might not do connection pooling by
    /// default. If set to `false`, we guarantee no connection pooling will happen.
    ///
    /// This setting is used by the Agent-level HTTP client.
    pub fn allow_connection_pooling(mut self, allow: bool) -> Self {
        self.allow_connection_pooling = allow;
        self
    }

    /// Build the [`crate::HttpClient`].
    ///
    /// Returns [`crate::HttpClientError::InvalidConfig`] if required fields
    /// (base URL, timeout) were not set.
    pub fn build(self) -> Result<crate::HttpClient, crate::HttpClientError> {
        let base_url = self.base_url.ok_or_else(|| {
            crate::HttpClientError::InvalidConfig("base_url is required".to_owned())
        })?;
        let timeout = self.timeout.ok_or_else(|| {
            crate::HttpClientError::InvalidConfig("timeout is required".to_owned())
        })?;
        let config = HttpClientConfig {
            base_url,
            timeout,
            treat_http_errors_as_errors: self.treat_http_errors_as_errors,
            retry: self.retry,
            allow_connection_pooling: self.allow_connection_pooling,
        };
        crate::HttpClient::from_config_and_transport(config, self.transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn config_getters() {
        let config =
            HttpClientConfig::new("http://localhost:8126".to_owned(), Duration::from_secs(3));
        assert_eq!(config.base_url(), "http://localhost:8126");
        assert_eq!(config.timeout(), Duration::from_secs(3));
        assert!(config.treat_http_errors_as_errors());
    }

    #[test]
    fn builder_missing_base_url() {
        let result = HttpClientBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("base_url is required"));
    }

    #[test]
    fn builder_missing_timeout() {
        let result = HttpClientBuilder::new()
            .base_url("http://localhost".to_owned())
            .build();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("timeout is required"));
    }

    #[test]
    fn builder_success() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost:8126".to_owned())
            .timeout(Duration::from_secs(3))
            .build();
        assert!(client.is_ok());
    }

    #[test]
    fn builder_treat_http_errors_defaults_true() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost".to_owned())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        assert!(client.config().treat_http_errors_as_errors());
    }

    #[test]
    fn builder_treat_http_errors_set_false() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost".to_owned())
            .timeout(Duration::from_secs(1))
            .treat_http_errors_as_errors(false)
            .build()
            .unwrap();
        assert!(!client.config().treat_http_errors_as_errors());
    }
}
