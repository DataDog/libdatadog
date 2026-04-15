// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Builder for [`crate::AgentClient`].

use std::collections::HashMap;
use std::time::Duration;

use libdd_http_client::RetryConfig;

use crate::{error::BuildError, language_metadata::LanguageMetadata, AgentClient};

/// Default timeout for agent requests: 2 000 ms.
///
/// Matches dd-trace-py's `DEFAULT_TIMEOUT = 2.0 s` (`constants.py:97`).
pub const DEFAULT_TIMEOUT_MS: u64 = 2_000;

/// Default retry configuration: 2 retries (3 total attempts), 100 ms initial delay,
/// exponential backoff with full jitter.
///
/// This approximates dd-trace-py's `fibonacci_backoff_with_jitter` pattern used in
/// `writer.py:245-249`, `stats.py:123-126`, and `datastreams/processor.py:140-143`.
pub fn default_retry_config() -> RetryConfig {
    todo!()
}

/// Transport configuration for the agent client.
///
/// Determines how the client connects to the Datadog agent (or an intake endpoint).
/// Set via [`AgentClientBuilder::transport`] or the convenience helpers
/// [`AgentClientBuilder::http`], [`AgentClientBuilder::https`],
/// [`AgentClientBuilder::unix_socket`], etc.
#[derive(Debug, Clone)]
pub enum AgentTransport {
    /// HTTP over TCP to `http://{host}:{port}`.
    Http {
        /// Hostname or IP address.
        host: String,
        /// Port number.
        port: u16,
    },
    /// HTTPS over TCP to `https://{host}:{port}` (e.g. for intake endpoints).
    Https {
        /// Hostname or IP address.
        host: String,
        /// Port number.
        port: u16,
    },
    /// Unix Domain Socket.
    ///
    /// HTTP requests are still formed with `Host: localhost`; the socket path
    /// governs only the transport layer.
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
    ///
    /// Mirrors the auto-detect logic in dd-trace-py's `_agent.py:32-49`.
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

/// Connection mode for the underlying HTTP client.
///
/// # Correctness note
///
/// The Datadog agent has a low keep-alive timeout that causes "pipe closed" errors on every
/// second connection when connection reuse is enabled. [`ClientMode::Periodic`] (the default)
/// disables connection pooling and is **correct** for all periodic-flush writers (traces, stats,
/// data streams). Only high-frequency continuous senders (e.g. a streaming profiling exporter)
/// should opt into [`ClientMode::Persistent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClientMode {
    /// No connection pooling. Correct for periodic flushes to the agent.
    #[default]
    Periodic,
    /// Keep connections alive across requests.
    ///
    /// Use only for high-frequency continuous senders.
    Persistent,
}

/// Builder for [`AgentClient`].
///
/// Obtain via [`AgentClient::builder`].
///
/// # Required fields
///
/// - Transport: set via [`AgentClientBuilder::transport`] or a convenience method
///   ([`AgentClientBuilder::http`], [`AgentClientBuilder::https`],
///   [`AgentClientBuilder::unix_socket`], [`AgentClientBuilder::windows_named_pipe`],
///   [`AgentClientBuilder::auto_detect`]).
/// - [`AgentClientBuilder::language_metadata`].
///
/// # Agentless mode
///
/// Call [`AgentClientBuilder::api_key`] with your Datadog API key and point the transport to
/// the intake endpoint via [`AgentClientBuilder::https`]. The client injects `dd-api-key` on
/// every request.
///
/// # Testing
///
/// Call [`AgentClientBuilder::test_token`] to inject `x-datadog-test-session-token` on every
/// request. This replaces dd-trace-py's `AgentWriter.set_test_session_token` (`writer.py:754-755`).
///
/// # Fork safety
///
/// The underlying `libdd-http-client` uses `hickory-dns` by default — an in-process, fork-safe
/// DNS resolver that avoids the class of bugs where a forked child inherits open sockets from a
/// parent's DNS thread pool. This is important for host processes that fork (Django, Flask,
/// Celery workers, PHP-FPM, etc.).
#[derive(Debug, Default)]
pub struct AgentClientBuilder {
    transport: Option<AgentTransport>,
    api_key: Option<String>,
    test_token: Option<String>,
    timeout: Option<Duration>,
    language: Option<LanguageMetadata>,
    retry: Option<RetryConfig>,
    client_mode: ClientMode,
    extra_headers: HashMap<String, String>,
}

impl AgentClientBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        todo!()
    }

    // ── Transport ─────────────────────────────────────────────────────────────

    /// Set the transport configuration.
    pub fn transport(self, transport: AgentTransport) -> Self {
        todo!()
    }

    /// Convenience: HTTP over TCP.
    pub fn http(self, host: impl Into<String>, port: u16) -> Self {
        todo!()
    }

    /// Convenience: HTTPS over TCP.
    pub fn https(self, host: impl Into<String>, port: u16) -> Self {
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
    ///
    /// Mirrors the logic in dd-trace-py's `_agent.py:32-49`.
    #[cfg(unix)]
    pub fn auto_detect(
        self,
        uds_path: impl Into<std::path::PathBuf>,
        fallback_host: impl Into<String>,
        fallback_port: u16,
    ) -> Self {
        todo!()
    }

    // ── Authentication / routing ──────────────────────────────────────────────

    /// Set the Datadog API key (agentless mode).
    ///
    /// When set, `dd-api-key: <key>` is injected on every request.
    /// Point the transport to the intake endpoint via [`AgentClientBuilder::https`].
    pub fn api_key(self, key: impl Into<String>) -> Self {
        todo!()
    }

    /// Set the test session token.
    ///
    /// When set, `x-datadog-test-session-token: <token>` is injected on every request.
    /// Replaces dd-trace-py's `AgentWriter.set_test_session_token` (`writer.py:754-755`).
    pub fn test_agent_session_token(self, token: impl Into<String>) -> Self {
        todo!()
    }

    // ── Timeout / retries ─────────────────────────────────────────────────────

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
    /// Defaults to [`default_retry_config`]: 2 retries, 100 ms initial delay, exponential
    /// backoff with full jitter.
    pub fn retry(self, config: RetryConfig) -> Self {
        todo!()
    }

    // ── Language metadata ─────────────────────────────────────────────────────

    /// Set the language/runtime metadata injected into every request.
    ///
    /// Required. Drives `Datadog-Meta-Lang`, `Datadog-Meta-Lang-Version`,
    /// `Datadog-Meta-Lang-Interpreter`, `Datadog-Meta-Tracer-Version`, and `User-Agent`.
    pub fn language_metadata(self, meta: LanguageMetadata) -> Self {
        todo!()
    }

    // ── Connection pooling ────────────────────────────────────────────────────

    /// Set the connection mode. Defaults to [`ClientMode::Periodic`].
    ///
    /// See [`ClientMode`] for the correctness rationale behind the default.
    pub fn client_mode(self, mode: ClientMode) -> Self {
        todo!()
    }

    // ── Compression ───────────────────────────────────────────────────────────
    //
    // Not exposed in v1. Gzip compression (level 6, matching dd-trace-py's trace writer at
    // `writer.py:490`) will be added in a follow-up once the core send paths are stable.
    // Per-method defaults (e.g. unconditional gzip for `send_pipeline_stats`) are already
    // baked in; only the opt-in client-level `gzip(level)` builder knob is deferred.

    // ── Extra headers ─────────────────────────────────────────────────────────

    /// Merge additional headers into every request.
    ///
    /// Intended for `_DD_TRACE_WRITER_ADDITIONAL_HEADERS` in dd-trace-py.
    pub fn extra_headers(self, headers: HashMap<String, String>) -> Self {
        todo!()
    }

    // ── Build ─────────────────────────────────────────────────────────────────

    /// Build the [`AgentClient`].
    ///
    /// # Errors
    ///
    /// - [`BuildError::MissingTransport`] — no transport was configured.
    /// - [`BuildError::MissingLanguageMetadata`] — no language metadata was configured.
    pub fn build(self) -> Result<AgentClient, BuildError> {
        todo!()
    }
}
