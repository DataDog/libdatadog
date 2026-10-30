// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTel thread-level context captured from the crashing process.
//!
//! Each entry represents the `otel_thread_ctx_v1` TLS record for one thread,
//! decoded from the crashing process's memory by the receiver.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A decoded OTel thread-level context from one thread in the crashing process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OtelThreadContext {
    /// Thread ID that owned this context.
    pub tid: i32,
    /// Thread name (from `/proc/<pid>/task/<tid>/comm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
    /// W3C trace-id as 32 lowercase hex characters.
    pub trace_id: String,
    /// Span-id as 16 lowercase hex characters.
    pub span_id: String,
    /// W3C trace-flags byte.
    pub trace_flags: u8,
    /// Decoded key-value attributes from the record's packed `attrs_data`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<OtelAttribute>,
}

/// A single key-value attribute from the OTel thread context record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OtelAttribute {
    /// Key index into the string table (0 = `local_root_span_id`).
    pub key_index: u8,
    /// The attribute value as a UTF-8 string.
    pub value: String,
}
