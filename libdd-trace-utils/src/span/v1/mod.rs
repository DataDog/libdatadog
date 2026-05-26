// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{BytesData, SliceData, TraceData};
use std::collections::HashMap;

/// OpenTelemetry SpanKind values, encoded on the wire as a `uint32`.
/// Unset or unrecognized kinds default to [`SpanKind::Internal`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanKind {
    #[default]
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

impl SpanKind {
    /// Parses a v0.4 `span.kind` meta value into a [`SpanKind`].
    /// Unrecognized values map to [`SpanKind::Internal`].
    pub fn from_meta(s: &str) -> Self {
        match s {
            "server" => SpanKind::Server,
            "client" => SpanKind::Client,
            "producer" => SpanKind::Producer,
            "consumer" => SpanKind::Consumer,
            _ => SpanKind::Internal,
        }
    }
}

/// Typed V1 attribute value.
/// Replaces v0.4's split `meta` / `metrics` / `meta_struct` maps.
#[derive(Debug, PartialEq)]
pub enum AttributeValue<T: TraceData> {
    String(T::Text),
    Float(f64),
    Int(i64),
    Bool(bool),
    Bytes(T::Bytes),
    KeyValue(HashMap<T::Text, AttributeValue<T>>),
    List(Vec<AttributeValue<T>>),
}

impl<T: TraceData> Clone for AttributeValue<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        match self {
            AttributeValue::String(v) => AttributeValue::String(v.clone()),
            AttributeValue::Float(v) => AttributeValue::Float(*v),
            AttributeValue::Int(v) => AttributeValue::Int(*v),
            AttributeValue::Bool(v) => AttributeValue::Bool(*v),
            AttributeValue::Bytes(v) => AttributeValue::Bytes(v.clone()),
            AttributeValue::KeyValue(m) => AttributeValue::KeyValue(m.clone()),
            AttributeValue::List(v) => AttributeValue::List(v.clone()),
        }
    }
}

/// The generic representation of a V1 span.
///
/// `T` is the type used to represent strings in the span, it can be either owned (e.g.
/// BytesString) or borrowed (e.g. &str). To define a generic function taking any `Span<T>` you can
/// use the [`TraceData`] trait:
/// ```
/// use libdd_trace_utils::span::{v1::Span, TraceData};
/// fn foo<T: TraceData>(span: Span<T>) {
///     let _ = span.attributes.get("foo");
/// }
/// ```
#[derive(Debug, PartialEq, Default)]
pub struct Span<T: TraceData> {
    pub service: T::Text,
    pub name: T::Text,
    pub resource: T::Text,
    pub r#type: T::Text,
    /// 128-bit trace ID stored as big-endian bytes.
    pub trace_id: [u8; 16],
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: bool,
    pub span_kind: SpanKind,
    pub env: T::Text,
    pub version: T::Text,
    pub component: T::Text,
    pub attributes: HashMap<T::Text, AttributeValue<T>>,
    pub span_links: Vec<SpanLink<T>>,
    pub span_events: Vec<SpanEvent<T>>,
}

impl<T: TraceData> Clone for Span<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        Span {
            service: self.service.clone(),
            name: self.name.clone(),
            resource: self.resource.clone(),
            r#type: self.r#type.clone(),
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_id: self.parent_id,
            start: self.start,
            duration: self.duration,
            error: self.error,
            span_kind: self.span_kind,
            env: self.env.clone(),
            version: self.version.clone(),
            component: self.component.clone(),
            attributes: self.attributes.clone(),
            span_links: self.span_links.clone(),
            span_events: self.span_events.clone(),
        }
    }
}

/// The generic representation of a V1 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug, PartialEq, Default)]
pub struct SpanLink<T: TraceData> {
    pub trace_id: [u8; 16],
    pub span_id: u64,
    pub attributes: HashMap<T::Text, AttributeValue<T>>,
    pub tracestate: T::Text,
    pub flags: u32,
}

impl<T: TraceData> Clone for SpanLink<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        SpanLink {
            trace_id: self.trace_id,
            span_id: self.span_id,
            attributes: self.attributes.clone(),
            tracestate: self.tracestate.clone(),
            flags: self.flags,
        }
    }
}

/// The generic representation of a V1 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug, PartialEq, Default)]
pub struct SpanEvent<T: TraceData> {
    pub time_unix_nano: u64,
    pub name: T::Text,
    pub attributes: HashMap<T::Text, AttributeValue<T>>,
}

impl<T: TraceData> Clone for SpanEvent<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        SpanEvent {
            time_unix_nano: self.time_unix_nano,
            name: self.name.clone(),
            attributes: self.attributes.clone(),
        }
    }
}

/// A V1 trace chunk: a group of spans sharing the same `trace_id`, plus chunk-level metadata.
#[derive(Debug, PartialEq, Default)]
pub struct TraceChunk<T: TraceData> {
    pub trace_id: [u8; 16],
    pub priority: Option<i32>,
    pub origin: Option<T::Text>,
    pub sampling_mechanism: Option<u32>,
    pub dropped_trace: bool,
    pub attributes: HashMap<T::Text, AttributeValue<T>>,
    pub spans: Vec<Span<T>>,
}

impl<T: TraceData> Clone for TraceChunk<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        TraceChunk {
            trace_id: self.trace_id,
            priority: self.priority,
            origin: self.origin.clone(),
            sampling_mechanism: self.sampling_mechanism,
            dropped_trace: self.dropped_trace,
            attributes: self.attributes.clone(),
            spans: self.spans.clone(),
        }
    }
}

/// A V1 tracer payload: tracer-level metadata and the trace chunks it carries.
#[derive(Debug, PartialEq, Default)]
pub struct TracerPayload<T: TraceData> {
    pub language_name: T::Text,
    pub language_version: T::Text,
    pub tracer_version: T::Text,
    pub runtime_id: T::Text,
    pub env: T::Text,
    pub hostname: T::Text,
    pub app_version: T::Text,
    pub attributes: HashMap<T::Text, AttributeValue<T>>,
    pub chunks: Vec<TraceChunk<T>>,
}

impl<T: TraceData> Clone for TracerPayload<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        TracerPayload {
            language_name: self.language_name.clone(),
            language_version: self.language_version.clone(),
            tracer_version: self.tracer_version.clone(),
            runtime_id: self.runtime_id.clone(),
            env: self.env.clone(),
            hostname: self.hostname.clone(),
            app_version: self.app_version.clone(),
            attributes: self.attributes.clone(),
            chunks: self.chunks.clone(),
        }
    }
}

pub type SpanBytes = Span<BytesData>;
pub type SpanLinkBytes = SpanLink<BytesData>;
pub type SpanEventBytes = SpanEvent<BytesData>;
pub type AttributeValueBytes = AttributeValue<BytesData>;
pub type TraceChunkBytes = TraceChunk<BytesData>;
pub type TracerPayloadBytes = TracerPayload<BytesData>;

pub type SpanSlice<'a> = Span<SliceData<'a>>;
pub type SpanLinkSlice<'a> = SpanLink<SliceData<'a>>;
pub type SpanEventSlice<'a> = SpanEvent<SliceData<'a>>;
pub type AttributeValueSlice<'a> = AttributeValue<SliceData<'a>>;
pub type TraceChunkSlice<'a> = TraceChunk<SliceData<'a>>;
pub type TracerPayloadSlice<'a> = TracerPayload<SliceData<'a>>;

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_tinybytes::BytesString;

    #[test]
    fn span_kind_default_is_internal() {
        assert_eq!(SpanKind::default(), SpanKind::Internal);
    }

    #[test]
    fn span_kind_from_meta() {
        assert_eq!(SpanKind::from_meta("server"), SpanKind::Server);
        assert_eq!(SpanKind::from_meta("client"), SpanKind::Client);
        assert_eq!(SpanKind::from_meta("producer"), SpanKind::Producer);
        assert_eq!(SpanKind::from_meta("consumer"), SpanKind::Consumer);
        assert_eq!(SpanKind::from_meta("internal"), SpanKind::Internal);
        assert_eq!(SpanKind::from_meta(""), SpanKind::Internal);
        assert_eq!(SpanKind::from_meta("anything-else"), SpanKind::Internal);
    }

    #[test]
    fn span_kind_repr_matches_otel_spec() {
        assert_eq!(SpanKind::Internal as u32, 1);
        assert_eq!(SpanKind::Server as u32, 2);
        assert_eq!(SpanKind::Client as u32, 3);
        assert_eq!(SpanKind::Producer as u32, 4);
        assert_eq!(SpanKind::Consumer as u32, 5);
    }

    #[test]
    fn span_default_has_zero_trace_id_and_internal_kind() {
        let s = SpanBytes::default();
        assert_eq!(s.trace_id, [0u8; 16]);
        assert_eq!(s.span_kind, SpanKind::Internal);
        assert!(!s.error);
        assert!(s.attributes.is_empty());
    }

    #[test]
    fn attribute_value_clone_preserves_variants() {
        let s = AttributeValueBytes::String(BytesString::from_static("v"));
        assert_eq!(s.clone(), s);
        let n = AttributeValueBytes::Int(42);
        assert_eq!(n.clone(), n);
        let list = AttributeValueBytes::List(vec![AttributeValueBytes::Bool(true)]);
        assert_eq!(list.clone(), list);
    }
}
