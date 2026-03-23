// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types specific to [`crate::AgentClient::send_traces`].

use std::collections::HashMap;

/// Wire format of the trace payload.
///
/// Determines both the `Content-Type` header and the target endpoint.
///
/// # Format selection
///
/// The caller is currently responsible for choosing the format. In practice this means
/// starting with [`TraceFormat::MsgpackV5`] and downgrading to [`TraceFormat::MsgpackV4`]
/// when the agent returns 404 or 415 (e.g. on Windows, or when AppSec/IAST is active) —
/// the same sticky downgrade that dd-trace-py performs in `AgentWriter` (`writer.py`).
///
/// In a future version this negotiation may be moved into the client itself so that format
/// selection becomes automatic and callers no longer need to track the downgrade state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFormat {
    /// `application/msgpack` to `/v0.5/traces`. Preferred format.
    MsgpackV5,
    /// `application/msgpack` to `/v0.4/traces`. Fallback for Windows / AppSec.
    MsgpackV4,
    /// `application/json` to `/v1/input`. Used in agentless mode.
    JsonV1,
}

/// Per-request options for [`crate::AgentClient::send_traces`].
#[derive(Debug, Clone, Default)]
pub struct TraceSendOptions {
    /// When `true`, appends `Datadog-Client-Computed-Top-Level: yes`.
    ///
    /// Signals to the agent that the client has already marked top-level spans, allowing the agent
    /// to skip its own top-level computation. In dd-trace-py this header is always set
    /// (`writer.py:643`); here it is opt-in so that callers that do not compute top-level spans
    /// can omit it.
    pub computed_top_level: bool,
}

/// Parsed response from the agent after a successful trace submission.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// HTTP status code returned by the agent.
    pub status: u16,
    /// Per-service sampling rates parsed from the `rate_by_service` field of the agent response
    /// body, if present. Mirrors the JSON parsing done in dd-trace-py at `writer.py:728-734`.
    pub rate_by_service: Option<HashMap<String, f64>>,
}
