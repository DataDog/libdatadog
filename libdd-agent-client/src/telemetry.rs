// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types specific to [`crate::AgentClient::send_telemetry`].

/// A single telemetry event to send via [`crate::AgentClient::send_telemetry`].
///
/// The three per-request headers — `DD-Telemetry-Request-Type`, `DD-Telemetry-API-Version`, and
/// `DD-Telemetry-Debug-Enabled` — are derived automatically from this struct, removing the
/// need for callers to build headers manually (as done in `telemetry/writer.py:111-117`).
///
/// Endpoint routing (agent proxy vs. agentless intake) is resolved by the client based on
/// whether an API key was set at build time, replacing the ad-hoc logic at
/// `telemetry/writer.py:119-129`.
#[derive(Debug, Clone)]
pub struct TelemetryRequest {
    /// Value for the `DD-Telemetry-Request-Type` header, e.g. `"app-started"`.
    pub request_type: String,
    /// Value for the `DD-Telemetry-API-Version` header, e.g. `"v2"`.
    pub api_version: String,
    /// When `true`, sets `DD-Telemetry-Debug-Enabled: true`.
    pub debug: bool,
    /// Pre-serialized JSON payload body.
    ///
    /// The caller is responsible for serializing the event body to JSON before constructing
    /// this struct. The client sends these bytes as-is with `Content-Type: application/json`.
    pub body: bytes::Bytes,
}
