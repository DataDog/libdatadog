// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct DebuggerPayload<'a> {
    pub service: Cow<'a, str>,
    pub ddsource: Cow<'static, str>,
    pub timestamp: u64,
    pub debugger: DebuggerData<'a>,
    pub message: Option<Cow<'a, str>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum DebuggerData<'a> {
    Snapshot(Snapshot<'a>),
    Diagnostics(Diagnostics<'a>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProbeMetadataLocation<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<Cow<'a, str>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProbeMetadata<'a> {
    pub id: Cow<'a, str>,
    pub location: ProbeMetadataLocation<'a>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotEvaluationError {
    pub expr: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotStackFrame {
    pub expr: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot<'a> {
    pub language: Cow<'a, str>,
    pub id: Cow<'a, str>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_capture_id: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_hash: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captures: Option<Captures<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeMetadata<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evaluation_errors: Vec<SnapshotEvaluationError>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<SnapshotStackFrame>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Captures<'a> {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub lines: HashMap<u32, Capture<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<Capture<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#return: Option<Capture<'a>>,
}

pub type Fields<'a> = HashMap<Cow<'a, str>, Value<'a>>;
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Capture<'a> {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(rename = "staticFields")]
    pub static_fields: Fields<'a>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub arguments: Fields<'a>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub locals: Fields<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throwable: Option<Value<'a>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry<'a>(pub Value<'a>, pub Value<'a>);

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Value<'a> {
    pub r#type: Cow<'a, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fields: Fields<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Value<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<Entry<'a>>,
    #[serde(skip_serializing_if = "<&bool as std::ops::Not>::not")]
    pub is_null: bool,
    #[serde(skip_serializing_if = "<&bool as std::ops::Not>::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_captured_reason: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<Cow<'a, str>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics<'a> {
    pub probe_id: Cow<'a, str>,
    pub runtime_id: Cow<'a, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Cow<'a, str>>,
    pub probe_version: u64,
    pub status: ProbeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<DiagnosticsError<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Cow<'a, str>>,
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, Eq, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
#[repr(C)]
pub enum ProbeStatus {
    #[default]
    Received,
    Installed,
    Emitting,
    Error,
    Blocked,
    Warning,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsError<'a> {
    pub r#type: Cow<'a, str>,
    pub message: Cow<'a, str>,
    pub stacktrace: Option<Cow<'a, str>>,
}
