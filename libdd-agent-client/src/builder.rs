// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Builder for [`crate::AgentClient`].

use std::collections::HashMap;
use std::time::Duration;

use libdd_http_client::RetryConfig;

use crate::{error::BuildError, language_metadata::LanguageMetadata, AgentClient};

/// Default timeout for agent requests.
pub const DEFAULT_TIMEOUT_MS: u64 = 2_000;

/// Default retry configuration: 2 retries (3 total attempts), 100 ms initial delay,
/// exponential backoff with full jitter.
pub fn default_retry_config() -> RetryConfig {
    RetryConfig::new()
        .max_retries(2)
        .initial_delay(Duration::from_millis(100))
        .with_jitter(true)
}

/// Transport configuration for the agent client.
///
/// Determines how the client connects to the Datadog agent.
/// Set via [`AgentClientBuilder::transport`] or the convenience helpers
/// [`AgentClientBuilder::http`], [`AgentClientBuilder::unix_socket`], etc.
#[derive(Debug, Clone)]
pub enum AgentTransport {
    /// HTTP over TCP.
    Http {
        /// Hostname or IP address.
        host: String,
        /// Port number.
        port: u16,
    },
    /// Unix Domain Socket.
    ///
    /// HTTP requests are still formed with `Host: localhost`. The socket path governs only the
    /// transport layer.
    #[cfg(unix)]
    UnixSocket {
        /// Filesystem path to the socket file.
        path: std::path::PathBuf,
    },
    /// Windows Named Pipe.
    #[cfg(windows)]
    NamedPipe {
        /// Named pipe path, e.g. `\\.\pipe\DD_APM_DRIVER`.
        path: std::ffi::OsString,
    },
    /// Probe at build time: use UDS if the socket file exists, otherwise fall back to HTTP.
    #[cfg(unix)]
    AutoDetect {
        /// UDS path to probe.
        uds_path: std::path::PathBuf,
        /// Fallback host when the socket is absent.
        fallback_host: String,
        /// Fallback port when the socket is absent (typically 8126).
        fallback_port: u16,
    },
}

impl Default for AgentTransport {
    fn default() -> Self {
        AgentTransport::Http {
            host: "localhost".to_string(),
            port: 8126,
        }
    }
}

/// Builder for [`AgentClient`].
///
/// Obtain via [`AgentClient::builder`].
///
/// # Required fields
///
/// - Transport: set via [`AgentClientBuilder::transport`] or a convenience method
///   ([`AgentClientBuilder::http`], [`AgentClientBuilder::unix_socket`],
///   [`AgentClientBuilder::windows_named_pipe`], [`AgentClientBuilder::auto_detect`]).
/// - [`AgentClientBuilder::language_metadata`].
///
/// # Test tokens
///
/// Call [`AgentClientBuilder::test_agent_session_token`] to inject
/// `x-datadog-test-session-token` on every request.
#[derive(Debug, Default)]
pub struct AgentClientBuilder {
    transport: Option<AgentTransport>,
    test_token: Option<String>,
    timeout: Option<Duration>,
    language: Option<LanguageMetadata>,
    retry: Option<RetryConfig>,
    keep_alive: bool,
    extra_headers: HashMap<String, String>,
}

impl AgentClientBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the transport configuration.
    pub fn transport(mut self, transport: AgentTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Convenience: HTTP over TCP.
    pub fn http(self, host: impl Into<String>, port: u16) -> Self {
        self.transport(AgentTransport::Http {
            host: host.into(),
            port,
        })
    }

    /// Convenience: Unix Domain Socket.
    #[cfg(unix)]
    pub fn unix_socket(self, path: impl Into<std::path::PathBuf>) -> Self {
        self.transport(AgentTransport::UnixSocket { path: path.into() })
    }

    /// Convenience: Windows Named Pipe.
    #[cfg(windows)]
    pub fn windows_named_pipe(self, path: impl Into<std::ffi::OsString>) -> Self {
        self.transport(AgentTransport::NamedPipe { path: path.into() })
    }

    /// Convenience: auto-detect transport (UDS if socket file exists, else HTTP).
    #[cfg(unix)]
    pub fn auto_detect(
        self,
        uds_path: impl Into<std::path::PathBuf>,
        fallback_host: impl Into<String>,
        fallback_port: u16,
    ) -> Self {
        self.transport(AgentTransport::AutoDetect {
            uds_path: uds_path.into(),
            fallback_host: fallback_host.into(),
            fallback_port,
        })
    }

    /// Set the test session token.
    ///
    /// When set, `x-datadog-test-session-token: <token>` is injected on every request.
    pub fn test_agent_session_token(mut self, token: impl Into<String>) -> Self {
        self.test_token = Some(token.into());
        self
    }

    /// Set the request timeout.
    ///
    /// Defaults to [`DEFAULT_TIMEOUT_MS`] (2 000 ms) when not set.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Read the timeout from `DD_TRACE_AGENT_TIMEOUT_SECONDS`, falling back to
    /// [`DEFAULT_TIMEOUT_MS`] if the variable is unset or unparseable.
    pub fn timeout_from_env(mut self) -> Self {
        let timeout = std::env::var("DD_TRACE_AGENT_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(|secs| Duration::from_millis((secs * 1000.0) as u64))
            .unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS));
        self.timeout = Some(timeout);
        self
    }

    /// Override the default retry configuration.
    ///
    /// Defaults to [`default_retry_config`].
    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
    }

    /// Set the language/runtime metadata injected into every request. Required.
    pub fn language_metadata(mut self, meta: LanguageMetadata) -> Self {
        self.language = Some(meta);
        self
    }

    /// Enable or disable HTTP keep-alive. Defaults to `false`.
    ///
    /// The Datadog agent has a low keep-alive timeout that causes "pipe closed" errors on every
    /// second connection when keep-alive is enabled. The default of `false` is correct for all
    /// periodic-flush writers (traces, stats, data streams). Set to `true` only for
    /// high-frequency continuous senders (e.g. a streaming profiling exporter).
    pub fn use_keep_alive(mut self, enabled: bool) -> Self {
        self.keep_alive = enabled;
        self
    }

    // Compression
    //
    // Not exposed in this libv1. Gzip compression (level 6, matching dd-trace-py's trace writer at
    // `writer.py:490`) will be added in a follow-up once the core send paths are stable.
    // Per-method defaults (e.g. unconditional gzip for `send_pipeline_stats`) are already
    // baked in; only the opt-in client-level `gzip(level)` builder knob is deferred.

    /// Additional custom headers to inject.
    pub fn extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Build the [`AgentClient`].
    pub fn build(self) -> Result<AgentClient, BuildError> {
        let transport = self.transport.ok_or(BuildError::MissingTransport)?;
        let language = self.language.ok_or(BuildError::MissingLanguageMetadata)?;
        let timeout = self
            .timeout
            .unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS));
        let retry = self.retry.unwrap_or_else(default_retry_config);

        // Resolve AutoDetect to a concrete transport.
        let resolved = resolve_transport(transport);

        // Build the underlying HTTP client.
        let http = build_http_client(resolved, timeout, retry)
            .map_err(|e| BuildError::HttpClient(e.to_string()))?;

        // Pre-compute all static headers that are injected on every request.
        let static_headers = build_static_headers(&language, self.test_token, self.extra_headers);

        Ok(AgentClient::new(http, static_headers))
    }
}

/// A resolved (concrete) transport — no AutoDetect.
pub(crate) enum ResolvedTransport {
    Http { host: String, port: u16 },
    #[cfg(unix)]
    UnixSocket { path: std::path::PathBuf },
    #[cfg(windows)]
    NamedPipe { path: std::ffi::OsString },
}

/// Resolve `AutoDetect` at build time; other variants pass through unchanged.
fn resolve_transport(transport: AgentTransport) -> ResolvedTransport {
    match transport {
        AgentTransport::Http { host, port } => ResolvedTransport::Http { host, port },
        #[cfg(unix)]
        AgentTransport::UnixSocket { path } => ResolvedTransport::UnixSocket { path },
        #[cfg(windows)]
        AgentTransport::NamedPipe { path } => ResolvedTransport::NamedPipe { path },
        #[cfg(unix)]
        AgentTransport::AutoDetect {
            uds_path,
            fallback_host,
            fallback_port,
        } => {
            if uds_path.exists() {
                ResolvedTransport::UnixSocket { path: uds_path }
            } else {
                ResolvedTransport::Http {
                    host: fallback_host,
                    port: fallback_port,
                }
            }
        }
    }
}

/// Derive the base URL for a resolved transport.
pub(crate) fn base_url_for(transport: &ResolvedTransport) -> String {
    match transport {
        ResolvedTransport::Http { host, port } => format!("http://{}:{}", host, port),
        #[cfg(unix)]
        ResolvedTransport::UnixSocket { .. } => "http://localhost".to_string(),
        #[cfg(windows)]
        ResolvedTransport::NamedPipe { .. } => "http://localhost".to_string(),
    }
}

fn build_http_client(
    transport: ResolvedTransport,
    timeout: Duration,
    retry: RetryConfig,
) -> Result<libdd_http_client::HttpClient, libdd_http_client::HttpClientError> {
    let base_url = base_url_for(&transport);
    let mut builder = libdd_http_client::HttpClient::builder()
        .base_url(base_url)
        .timeout(timeout)
        // HTTP errors are handled by each send method, not by the underlying client.
        // This allows methods like `agent_info` to interpret 404 as Ok(None) rather than
        // an error, and avoids retrying on HTTP 4xx/5xx.
        .treat_http_errors_as_errors(false)
        .retry(retry);

    match transport {
        ResolvedTransport::Http { .. } => {}
        #[cfg(unix)]
        ResolvedTransport::UnixSocket { path } => {
            builder = builder.unix_socket(path);
        }
        #[cfg(windows)]
        ResolvedTransport::NamedPipe { path } => {
            builder = builder.windows_named_pipe(path);
        }
    }

    builder.build()
}

fn build_static_headers(
    language: &LanguageMetadata,
    test_token: Option<String>,
    extra_headers: HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();

    headers.push(("Datadog-Meta-Lang".to_string(), language.language.clone()));
    headers.push((
        "Datadog-Meta-Lang-Version".to_string(),
        language.language_version.clone(),
    ));
    headers.push((
        "Datadog-Meta-Lang-Interpreter".to_string(),
        language.interpreter.clone(),
    ));
    headers.push((
        "Datadog-Meta-Tracer-Version".to_string(),
        language.tracer_version.clone(),
    ));
    headers.push(("User-Agent".to_string(), language.user_agent()));

    if let Some(token) = test_token {
        headers.push(("x-datadog-test-session-token".to_string(), token));
    }

    headers.extend(container_headers());
    headers.extend(extra_headers);

    headers
}

/// Read container / entity-ID headers from the host environment.
///
/// On Linux, parses `/proc/self/cgroup` to extract the container ID and injects
/// `Datadog-Container-Id` and `Datadog-Entity-ID`. Always injects `Datadog-External-Env`
/// when `DD_EXTERNAL_ENV` is set.
fn container_headers() -> Vec<(String, String)> {
    let mut headers = Vec::new();

    if let Ok(env) = std::env::var("DD_EXTERNAL_ENV") {
        if !env.is_empty() {
            headers.push(("Datadog-External-Env".to_string(), env));
        }
    }

    #[cfg(target_os = "linux")]
    if let Some(container_id) = read_container_id_from_cgroup() {
        let entity_id = format!("ci-{}", container_id);
        headers.push(("Datadog-Container-Id".to_string(), container_id));
        headers.push(("Datadog-Entity-ID".to_string(), entity_id));
    }

    headers
}

/// Parse a 64-character hex container ID from `/proc/self/cgroup`.
///
/// cgroup v1 paths end with the container ID, e.g.:
///   `12:blkio:/docker/abc123...64hex...`
#[cfg(target_os = "linux")]
fn read_container_id_from_cgroup() -> Option<String> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in content.lines() {
        // Each cgroup line is: <hierarchy-id>:<subsystem>:<path>
        let path = line.splitn(3, ':').nth(2)?;
        // Container ID is a 64-char hex segment at the end of the cgroup path.
        for segment in path.split('/').rev() {
            if segment.len() == 64 && segment.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(segment.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transport_is_localhost_8126() {
        let t = AgentTransport::default();
        match t {
            AgentTransport::Http { host, port } => {
                assert_eq!(host, "localhost");
                assert_eq!(port, 8126);
            }
            #[allow(unreachable_patterns)]
            _ => panic!("unexpected default transport"),
        }
    }

    #[test]
    fn default_retry_config_is_constructable() {
        // Just verify default_retry_config() doesn't panic.
        let _cfg = default_retry_config();
    }

    #[test]
    fn builder_new_is_default() {
        let b = AgentClientBuilder::new();
        assert!(b.transport.is_none());
        assert!(b.language.is_none());
        assert!(!b.keep_alive);
    }

    #[test]
    fn build_fails_without_transport() {
        let result = AgentClientBuilder::new()
            .language_metadata(LanguageMetadata::new("python", "3.12", "CPython", "2.0"))
            .build();
        assert!(matches!(result, Err(BuildError::MissingTransport)));
    }

    #[test]
    fn build_fails_without_language_metadata() {
        let result = AgentClientBuilder::new().http("localhost", 8126).build();
        assert!(matches!(result, Err(BuildError::MissingLanguageMetadata)));
    }

    #[test]
    fn build_succeeds_with_required_fields() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let result = AgentClientBuilder::new()
            .http("localhost", 8126)
            .language_metadata(LanguageMetadata::new("python", "3.12", "CPython", "2.0"))
            .build();
        assert!(result.is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn timeout_from_env_uses_default_when_unset() {
        std::env::remove_var("DD_TRACE_AGENT_TIMEOUT_SECONDS");
        let b = AgentClientBuilder::new().timeout_from_env();
        assert_eq!(
            b.timeout,
            Some(Duration::from_millis(DEFAULT_TIMEOUT_MS))
        );
    }

    #[test]
    #[serial_test::serial]
    fn timeout_from_env_parses_env_var() {
        std::env::set_var("DD_TRACE_AGENT_TIMEOUT_SECONDS", "5");
        let b = AgentClientBuilder::new().timeout_from_env();
        std::env::remove_var("DD_TRACE_AGENT_TIMEOUT_SECONDS");
        assert_eq!(b.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn extra_headers_stored() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let b = AgentClientBuilder::new().extra_headers(headers);
        assert_eq!(b.extra_headers.get("X-Custom").map(|s| s.as_str()), Some("value"));
    }
}
