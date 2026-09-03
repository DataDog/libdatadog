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

/// Idle timeout for connections kept alive by a client configured for periodic use (see
/// [`HttpClientBuilder::periodic`]).
///
/// Kept much smaller than typical keep-alive timeouts on the receiving end (e.g. the Datadog
/// agent), so that an idle pooled connection is dropped by our side before the receiver closes it.
pub(crate) const PERIODIC_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Max number of idle connections kept in a client's connection pool. This is a safety resource
/// bound that we don't really expect to hit in practice.
pub(crate) const POOL_MAX_IDLE: usize = 20;

/// Configuration for an [`crate::HttpClient`] instance.
///
/// Constructed via [`crate::HttpClient::new`] or [`HttpClientBuilder::build`].
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    base_url: String,
    timeout: Duration,
    treat_http_errors_as_errors: bool,
    retry: Option<RetryConfig>,
    /// If this client is setup for periodic flushes. This mostly affects connection pooling, see
    /// [`HttpClientBuilder::periodic`].
    periodic: bool,
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
            periodic: false,
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

    /// If this client is setup for periodic flushes. See [HttpClientBuilder::periodic].
    pub fn periodic(&self) -> bool {
        self.periodic
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
    periodic: bool,
}

impl Default for HttpClientBuilder {
    fn default() -> Self {
        Self {
            base_url: Default::default(),
            timeout: Default::default(),
            treat_http_errors_as_errors: true,
            retry: Default::default(),
            transport: Default::default(),
            periodic: false,
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

    /// Set whether this client is used for periodic one-shot communication (typically
    /// regularly flushing to the agent or to the backend). Defaults to `false`.
    ///
    /// This setting sets the lifetime of pooled connections to a timeout much smaller than 60s
    /// (e.g. 5s).
    ///
    /// The rationale for having short-lived connection pooling is that we've experienced races when
    /// the connection pooling timeout is higher than the keep-alive timeout of the receiving end.
    /// It's then possible to pick an idle connection and start a request while the connection get
    /// closed at the same time by the receiver, causing an error.
    ///
    /// Connection pooling was initially entirely disabled by this setting, but it happens that we
    /// send multiple separate requests in a short span of time (e.g. for telemetry on short-lived
    /// apps). In that situation, if we're agentless, making separate HTTPS connections is quite
    /// costly (can be on the order of magnitude of 0.5sec per connection). Having pooling with a
    /// short lifetime is a better choice, since we can reuse the same connection for those multiple
    /// consecutive requests, while avoiding the race condition.
    pub fn periodic(mut self, periodic: bool) -> Self {
        self.periodic = periodic;
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
            periodic: self.periodic,
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

    #[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
    #[test]
    fn builder_success() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost:8126".to_owned())
            .timeout(Duration::from_secs(3))
            .build();
        assert!(client.is_ok());
    }

    #[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
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

    #[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
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

    #[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
    #[test]
    fn builder_periodic_defaults_false() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost".to_owned())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        assert!(!client.config().periodic());
    }

    #[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
    #[test]
    fn builder_periodic_set_true() {
        ensure_crypto_provider();
        let client = HttpClientBuilder::new()
            .base_url("http://localhost".to_owned())
            .timeout(Duration::from_secs(1))
            .periodic(true)
            .build()
            .unwrap();
        assert!(client.config().periodic());
    }
}
