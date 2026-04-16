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
    todo!()
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
        todo!()
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
        todo!()
    }

    /// Set the transport configuration.
    pub fn transport(self, transport: AgentTransport) -> Self {
        todo!()
    }

    /// Convenience: HTTP over TCP.
    pub fn http(self, host: impl Into<String>, port: u16) -> Self {
        todo!()
    }

    /// Convenience: Unix Domain Socket.
    #[cfg(unix)]
    pub fn unix_socket(self, path: impl Into<std::path::PathBuf>) -> Self {
        todo!()
    }

    /// Convenience: Windows Named Pipe.
    #[cfg(windows)]
    pub fn windows_named_pipe(self, path: impl Into<std::ffi::OsString>) -> Self {
        todo!()
    }

    /// Convenience: auto-detect transport (UDS if socket file exists, else HTTP).
    #[cfg(unix)]
    pub fn auto_detect(
        self,
        uds_path: impl Into<std::path::PathBuf>,
        fallback_host: impl Into<String>,
        fallback_port: u16,
    ) -> Self {
        todo!()
    }

    /// Set the test session token.
    ///
    /// When set, `x-datadog-test-session-token: <token>` is injected on every request.
    pub fn test_agent_session_token(self, token: impl Into<String>) -> Self {
        todo!()
    }

    /// Set the request timeout.
    ///
    /// Defaults to [`DEFAULT_TIMEOUT_MS`] (2 000 ms) when not set.
    pub fn timeout(self, timeout: Duration) -> Self {
        todo!()
    }

    /// Read the timeout from `DD_TRACE_AGENT_TIMEOUT_SECONDS`, falling back to
    /// [`DEFAULT_TIMEOUT_MS`] if the variable is unset or unparseable.
    pub fn timeout_from_env(self) -> Self {
        todo!()
    }

    /// Override the default retry configuration.
    ///
    /// Defaults to [`default_retry_config`].
    pub fn retry(self, config: RetryConfig) -> Self {
        todo!()
    }

    /// Set the language/runtime metadata injected into every request. Required.
    pub fn language_metadata(self, meta: LanguageMetadata) -> Self {
        todo!()
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
    pub fn extra_headers(self, headers: HashMap<String, String>) -> Self {
        todo!()
    }

    /// Build the [`AgentClient`].
    pub fn build(self) -> Result<AgentClient, BuildError> {
        todo!()
    }
}
