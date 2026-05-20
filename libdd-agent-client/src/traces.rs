// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types specific to [`crate::AgentClient::send_traces`].

use std::collections::HashMap;

/// Wire format of the trace payload.
///
/// Determines both the `Content-Type` header and the target endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFormat {
    /// `application/msgpack` to `/v0.5/traces`. Preferred format.
    MsgpackV5,
    /// `application/msgpack` to `/v0.4/traces`. Fallback for Windows / AppSec.
    MsgpackV4,
}

/// Per-request options for [`crate::AgentClient::send_traces`].
#[derive(Debug, Clone, Default)]
pub struct TraceSendOptions {
    /// When `true`, appends `Datadog-Client-Computed-Top-Level: yes`.
    ///
    /// Signals to the agent that the client has already marked top-level spans, allowing the agent
    /// to skip its own top-level computation.
    pub computed_top_level: bool,
    /// When `true`, appends `Datadog-Client-Computed-Stats: yes`.
    ///
    /// Signals to the agent that the client has already computed APM stats for these traces,
    /// allowing the agent to skip its own stats computation.
    pub client_computed_stats: bool,
}

/// Parsed response from the agent after a successful trace submission.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// HTTP status code returned by the agent.
    pub status: u16,
    /// Per-service sampling rates parsed from the `rate_by_service` field of the agent response
    /// body, if present.
    pub rate_by_service: Option<HashMap<String, f64>>,
}
