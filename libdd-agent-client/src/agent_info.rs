// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types for [`crate::AgentClient::agent_info`].

/// Parsed response from a `GET /info` probe.
///
/// Returned by [`crate::AgentClient::agent_info`]. Contains agent capabilities and the
/// headers that dd-trace-py currently processes via the side-effectful `process_info_headers`
/// function (`agent.py:17-23`) — here they are explicit typed fields instead.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Available agent endpoints, e.g. `["/v0.4/traces", "/v0.5/traces"]`.
    pub endpoints: Vec<String>,
    /// Whether the agent supports client-side P0 dropping.
    pub client_drop_p0s: bool,
    /// Raw agent configuration block.
    pub config: serde_json::Value,
    /// Agent version string, if reported.
    pub version: Option<String>,
    /// Parsed from the `Datadog-Container-Tags-Hash` response header.
    ///
    /// Used by dd-trace-py to compute the base tag hash (`agent.py:17-23`).
    pub container_tags_hash: Option<String>,
    /// Value of the `Datadog-Agent-State` response header from the last `/info` fetch.
    ///
    /// The agent updates this opaque token whenever its internal state changes (e.g. a
    /// configuration reload). Clients that poll `/info` periodically can skip re-parsing
    /// the response body by comparing this value to the one returned by the previous call
    /// and only acting when it differs.
    pub state_hash: Option<String>,
}
