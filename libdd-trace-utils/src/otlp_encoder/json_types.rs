// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Minimal serde types for OTLP HTTP/JSON export (ExportTraceServiceRequest).
//!
//! These types mirror the OTLP protobuf schema for the HTTP/JSON wire format. Field names use
//! lowerCamelCase per the Protocol Buffers JSON Mapping spec; trace/span IDs are hex-encoded
//! strings; enum values (SpanKind, StatusCode) are integers.
//!
//! The canonical definitions live in the opentelemetry-proto repository:
//!   <https://github.com/open-telemetry/opentelemetry-proto/blob/v1.5.0/opentelemetry/proto/trace/v1/trace.proto>
//!   <https://github.com/open-telemetry/opentelemetry-proto/blob/v1.5.0/opentelemetry/proto/common/v1/common.proto>
//!
//! Hand-rolled serde structs are intentional here: for HTTP/JSON export, duplicating the type
//! definitions is simpler than pulling in `prost`-generated types from the `opentelemetry-proto`
//! crate. When HTTP/protobuf export is added, `opentelemetry-proto` should be introduced as a
//! dependency for that purpose:
//!   <https://github.com/open-telemetry/opentelemetry-rust/tree/opentelemetry-proto-0.28.0/opentelemetry-proto>

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Serialize, Serializer};

/// Top-level OTLP trace export request (ExportTraceServiceRequest).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTraceServiceRequest {
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    pub resource: Option<Resource>,
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Default, Serialize)]
pub struct Resource {
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpans {
    pub scope: Option<InstrumentationScope>,
    pub spans: Vec<OtlpSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_url: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpSpan {
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
    pub name: String,
    pub kind: i32,
    pub start_time_unix_nano: String,
    pub end_time_unix_nano: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<OtlpSpanLink>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<OtlpSpanEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_attributes_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_events_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpSpanLink {
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_attributes_count: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpSpanEvent {
    pub time_unix_nano: String,
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_attributes_count: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    pub value: AnyValue,
}

/// A typed value in an OTLP attribute. Each variant serializes as a single-key JSON object
/// matching the OTLP HTTP/JSON wire format (e.g. `{"stringValue":"hello"}`).
///
/// Per the protobuf JSON mapping spec, `int64` values must be encoded as strings to avoid
/// precision loss (JSON numbers are IEEE 754 doubles, exact only up to 2^53).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnyValue {
    StringValue(String),
    BoolValue(bool),
    #[serde(serialize_with = "serialize_int_value_as_string")]
    IntValue(i64),
    DoubleValue(f64),
    #[serde(serialize_with = "serialize_bytes_as_base64")]
    BytesValue(Vec<u8>),
    ArrayValue(ArrayValue),
}

fn serialize_int_value_as_string<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

fn serialize_bytes_as_base64<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&STANDARD.encode(v))
}

/// OTLP array value — wraps a list of [`AnyValue`] items.
#[derive(Debug, Serialize)]
pub struct ArrayValue {
    pub values: Vec<AnyValue>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub code: i32,
}

/// OTLP SpanKind enum values.
pub mod span_kind {
    pub const UNSPECIFIED: i32 = 0;
    pub const INTERNAL: i32 = 1;
    pub const SERVER: i32 = 2;
    pub const CLIENT: i32 = 3;
    pub const PRODUCER: i32 = 4;
    pub const CONSUMER: i32 = 5;
}

/// OTLP StatusCode enum values.
pub mod status_code {
    pub const UNSET: i32 = 0;
    pub const OK: i32 = 1;
    pub const ERROR: i32 = 2;
}
