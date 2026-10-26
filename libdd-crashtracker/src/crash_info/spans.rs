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

/// A snapshot of an active APM span captured at crash time by some collector.
/// When present, the receiver submits it as a finished span to the Datadog
/// Agent trace intake. See RFC 0016.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApmSpanSnapshot {
    pub schema_version: u32,
    /// Full 128-bit trace ID as a decimal string.
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: String,
    pub start_unix_ns: String,
    pub duration_ns: String,
    #[serde(default)]
    pub error: i32,
    #[serde(default)]
    pub has_sampling_priority: bool,
    #[serde(default)]
    pub sampling_priority: i32,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub resource: String,
    #[serde(rename = "type", default)]
    pub span_type: String,
}
