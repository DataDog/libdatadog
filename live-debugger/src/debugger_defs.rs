// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct DebuggerPayload<'a> {
    pub service: Cow<'a, str>,
    pub source: &'static str,
    pub timestamp: u64,
    pub debugger: DebuggerData<'a>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DebuggerData<'a> {
    pub snapshot: Snapshot<'a>,
}

#[derive(Serialize, Deserialize)]
pub struct ProbeMetadataLocation<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<Cow<'a, str>>,
}

#[derive(Serialize, Deserialize)]
pub struct ProbeMetadata<'a> {
    pub id: Cow<'a, str>,
    pub location: ProbeMetadataLocation<'a>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotEvaluationError {
    pub expr: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
pub struct SnapshotStackFrame {
    pub expr: String,
    pub message: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Snapshot<'a> {
    pub language: Cow<'a, str>,
    pub id: Cow<'a, str>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "exception-id")]
    pub exception_id: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captures: Option<Captures<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeMetadata<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "evaluationErrors")]
    pub evaluation_errors: Vec<SnapshotEvaluationError>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<SnapshotStackFrame>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Captures<'a> {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub lines: HashMap<u32, Capture<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<Capture<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#return: Option<Capture<'a>>,
}

pub type Fields<'a> = HashMap<Cow<'a, str>, Value<'a>>;
#[derive(Default, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
pub struct Entry<'a>(pub Value<'a>, pub Value<'a>);

#[derive(Default, Serialize, Deserialize)]
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
    #[serde(rename = "isNull")]
    pub is_null: bool,
    #[serde(skip_serializing_if = "<&bool as std::ops::Not>::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "notCapturedReason")]
    pub not_captured_reason: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<Cow<'a, str>>,
}
