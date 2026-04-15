// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! [`AgentClient`] and its send methods.

use bytes::Bytes;

use crate::{
    agent_info::AgentInfo,
    builder::AgentClientBuilder,
    error::SendError,
    telemetry::TelemetryRequest,
    traces::{AgentResponse, TraceFormat, TraceSendOptions},
};

/// A Datadog-agent-specialized HTTP client.
///
/// Wraps a configured [`libdd_http_client::HttpClient`] and injects Datadog-specific headers
/// automatically on every request:
///
/// - Language metadata headers (`Datadog-Meta-Lang`, `Datadog-Meta-Lang-Version`,
///   `Datadog-Meta-Lang-Interpreter`, `Datadog-Meta-Tracer-Version`) from the [`LanguageMetadata`]
///   supplied at build time.
/// - `User-Agent` derived from [`LanguageMetadata::user_agent`].
/// - Container/entity-ID headers (`Datadog-Container-Id`, `Datadog-Entity-ID`,
///   `Datadog-External-Env`) read from `/proc/self/cgroup` at startup, equivalent to dd-trace-py's
///   `container.update_headers()` (`container.py:157-183`).
/// - `dd-api-key` when an API key was set (agentless mode).
/// - `x-datadog-test-session-token` when a test token was set.
/// - Any extra headers registered via [`AgentClientBuilder::extra_headers`].
///
/// Obtain via [`AgentClient::builder`].
///
/// [`LanguageMetadata`]: crate::LanguageMetadata
pub struct AgentClient {
    // Opaque — fields are an implementation detail.
}

impl AgentClient {
    /// Create a new [`AgentClientBuilder`].
    pub fn builder() -> AgentClientBuilder {
        todo!()
    }

    /// Send a serialised trace payload to the agent.
    ///
    /// # Automatically injected headers
    ///
    /// - `X-Datadog-Trace-Count: <trace_count>` (per-payload — `writer.py:749-752`)
    /// - `Datadog-Send-Real-Http-Status: true` — instructs the agent to return 429 when it drops a
    ///   payload, rather than silently returning 200. dd-trace-py never sets this header, causing
    ///   silent drops that are invisible to the caller.
    /// - `Datadog-Client-Computed-Top-Level: yes` when [`TraceSendOptions::computed_top_level`] is
    ///   `true`.
    /// - Language metadata headers + container headers (see type-level docs).
    /// - `Content-Type` (`application/msgpack`) and endpoint path derived from `format`.
    /// - `Content-Encoding: gzip` when compression is enabled.
    ///
    /// # Returns
    ///
    /// An [`AgentResponse`] with the HTTP status and the parsed `rate_by_service` sampling
    /// rates from the agent response body (`writer.py:728-734`).
    pub async fn send_traces(
        &self,
        payload: Bytes,
        trace_count: usize,
        format: TraceFormat,
        opts: TraceSendOptions,
    ) -> Result<AgentResponse, SendError> {
        todo!()
    }

    /// Send span stats (APM concentrator buckets) to `/v0.6/stats`.
    ///
    /// `Content-Type` is always `application/msgpack`. Replaces the manual
    /// `get_connection` + raw `PUT` in `SpanStatsProcessor._flush_stats` (`stats.py:204-228`).
    pub async fn send_stats(&self, payload: Bytes) -> Result<(), SendError> {
        todo!()
    }

    /// Send data-streams pipeline stats to `/v0.1/pipeline_stats`.
    ///
    /// The payload is **always** gzip-compressed regardless of the client-level compression
    /// setting. This is a protocol requirement of the data-streams endpoint
    /// (`datastreams/processor.py:132`) and must not be a caller responsibility.
    pub async fn send_pipeline_stats(&self, payload: Bytes) -> Result<(), SendError> {
        todo!()
    }

    /// Send a telemetry event to the agent's telemetry proxy (`telemetry/proxy/api/v2/apmtelemetry`).
    ///
    /// Per-request headers `DD-Telemetry-Request-Type`, `DD-Telemetry-API-Version`, and
    /// `DD-Telemetry-Debug-Enabled` are injected automatically from `req`, replacing the
    /// manual construction in `_TelemetryClient.get_headers` (`telemetry/writer.py:111-117`).
    pub async fn send_telemetry(&self, req: TelemetryRequest) -> Result<(), SendError> {
        todo!()
    }

    /// Send an event via the agent's EVP (Event Platform) proxy.
    ///
    /// The agent forwards the request to `<subdomain>.datadoghq.com<path>`. `subdomain`
    /// controls the target intake (injected as `X-Datadog-EVP-Subdomain`); `path` is the
    /// endpoint on that intake (e.g. `/api/v2/exposures`).
    ///
    /// In dd-trace-py's openfeature writer both values are hardcoded constants
    /// (`openfeature/writer.py:24-27`), but they are independent routing dimensions and
    /// must both be supplied by the caller.
    pub async fn send_evp_event(
        &self,
        subdomain: &str,
        path: &str,
        payload: Bytes,
        content_type: &str,
    ) -> Result<(), SendError> {
        todo!()
    }

    /// Probe `GET /info` and return parsed agent capabilities.
    ///
    /// Processes the `Datadog-Container-Tags-Hash` response header and exposes it as
    /// [`AgentInfo::container_tags_hash`] rather than as a side-effect (as in dd-trace-py's
    /// `process_info_headers` at `agent.py:17-23`).
    ///
    /// Returns `Ok(None)` when the agent returns 404 (remote-config / info not supported).
    pub async fn agent_info(&self) -> Result<Option<AgentInfo>, SendError> {
        todo!()
    }
}
