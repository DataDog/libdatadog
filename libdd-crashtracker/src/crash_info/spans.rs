// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
}

/// A pre-encoded v0.4 msgpack trace payload captured at crash time by the
/// collector. The receiver hex-decodes it and POSTs the raw bytes to the
/// agent. See RFC 0016.
#[derive(Debug, Clone, PartialEq)]
pub struct ApmTracePayload {
    /// The agent URL to POST the payload to (e.g. "http://127.0.0.1:8126").
    pub agent_url: String,
    /// Raw v0.4 msgpack bytes (hex-decoded from the wire protocol).
    pub data: Vec<u8>,
}
