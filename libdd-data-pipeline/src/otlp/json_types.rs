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
//! The Rust implementation in opentelemetry-rust uses `prost`-generated types with an optional
//! `with-serde` feature (`opentelemetry-proto` crate). We use hand-rolled serde structs here to
//! avoid the `prost` + `tonic` dependency tree in this early implementation. If/when protobuf
//! support is added, these types should be replaced with `opentelemetry-proto`:
//!   <https://github.com/open-telemetry/opentelemetry-rust/tree/opentelemetry-proto-0.28.0/opentelemetry-proto>

use serde::Serialize;

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnyValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bool_value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub int_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_value: Option<String>,
}

impl AnyValue {
    pub fn string(s: String) -> Self {
        AnyValue {
            string_value: Some(s),
            bool_value: None,
            int_value: None,
            double_value: None,
            bytes_value: None,
        }
    }
    pub fn int(i: i64) -> Self {
        AnyValue {
            string_value: None,
            bool_value: None,
            int_value: Some(i),
            double_value: None,
            bytes_value: None,
        }
    }
    pub fn double(d: f64) -> Self {
        AnyValue {
            string_value: None,
            bool_value: None,
            int_value: None,
            double_value: Some(d),
            bytes_value: None,
        }
    }
    pub fn bool(b: bool) -> Self {
        AnyValue {
            string_value: None,
            bool_value: Some(b),
            int_value: None,
            double_value: None,
            bytes_value: None,
        }
    }
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
