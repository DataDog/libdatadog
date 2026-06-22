// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;
use crate::span::{BytesData, SliceData, TraceData};
pub use thin_vec::ThinVec;

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
#[derive(Debug)]
pub enum AttributeValue<T: TraceData> {
    String(T::Text),
    Float(f64),
    Int(i64),
    Bool(bool),
    Bytes(T::Bytes),
    KeyValue(VecMap<T::Text, AttributeValue<T>>),
    List(Vec<AttributeValue<T>>),
}

// `#[derive(PartialEq)]` only bounds the type parameter `T` itself, not the associated types
// (`T::Text`, `T::Bytes`) actually used in the fields below, so it can't be used here.
//
// `VecMap`'s own `PartialEq` impl is cfg-gated to `test`/`test-utils` (it allocates two
// `HashMap`s), so the `KeyValue` variant below can't just delegate to `VecMap::eq` — it
// reimplements the same last-write-wins comparison directly instead.
impl<T: TraceData> PartialEq for AttributeValue<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::KeyValue(a), Self::KeyValue(b)) => {
                let lhs: std::collections::HashMap<&T::Text, &AttributeValue<T>> =
                    a.iter().map(|(k, v)| (k, v)).collect();
                let rhs: std::collections::HashMap<&T::Text, &AttributeValue<T>> =
                    b.iter().map(|(k, v)| (k, v)).collect();
                lhs == rhs
            }
            (Self::List(a), Self::List(b)) => a == b,
            _ => false,
        }
    }
}

/// The generic representation of a V1 span.
///
/// `T: TraceData` carries the associated text type `T::Text` used for every string field in the
/// span; `T::Text` can be either owned (e.g. [`BytesString`](libdd_tinybytes::BytesString)) or
/// borrowed (e.g. `&str`). To define a generic function taking any `Span<T>` you can use the
/// [`TraceData`] trait:
/// ```
/// use libdd_trace_utils::span::{v1::Span, TraceData};
/// fn foo<T: TraceData>(span: Span<T>) {
///     let _ = span.attributes.get("foo");
/// }
/// ```
#[derive(Debug, Default)]
pub struct Span<T: TraceData> {
    pub service: T::Text,
    pub name: T::Text,
    pub resource: T::Text,
    pub r#type: T::Text,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: bool,
    pub span_kind: SpanKind,
    pub env: T::Text,
    pub version: T::Text,
    pub component: T::Text,
    pub attributes: VecMap<T::Text, AttributeValue<T>>,
    pub span_links: ThinVec<SpanLink<T>>,
    pub span_events: ThinVec<SpanEvent<T>>,
}

/// The generic representation of a V1 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug, Default)]
pub struct SpanLink<T: TraceData> {
    pub trace_id: [u8; 16],
    pub span_id: u64,
    pub attributes: VecMap<T::Text, AttributeValue<T>>,
    pub tracestate: T::Text,
    pub flags: u32,
}

/// The generic representation of a V1 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug, Default)]
pub struct SpanEvent<T: TraceData> {
    pub time_unix_nano: u64,
    pub name: T::Text,
    pub attributes: VecMap<T::Text, AttributeValue<T>>,
}

/// A V1 trace chunk: a group of spans sharing the same `trace_id`, plus chunk-level metadata.
#[derive(Debug, Default)]
pub struct TraceChunk<T: TraceData> {
    pub trace_id: [u8; 16],
    pub priority: Option<i32>,
    pub origin: T::Text,
    pub sampling_mechanism: Option<u32>,
    pub dropped_trace: bool,
    pub attributes: VecMap<T::Text, AttributeValue<T>>,
    pub spans: Vec<Span<T>>,
}

/// A V1 tracer payload: tracer-level metadata and the trace chunks it carries.
#[derive(Debug, Default)]
pub struct TracerPayload<T: TraceData> {
    pub container_id: T::Text,
    pub language_name: T::Text,
    pub language_version: T::Text,
    pub tracer_version: T::Text,
    pub runtime_id: T::Text,
    pub env: T::Text,
    pub hostname: T::Text,
    pub app_version: T::Text,
    pub attributes: VecMap<T::Text, AttributeValue<T>>,
    pub chunks: Vec<TraceChunk<T>>,
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
    fn span_default_has_internal_kind() {
        let s = SpanBytes::default();
        assert_eq!(s.span_kind, SpanKind::Internal);
        assert!(!s.error);
        assert!(s.attributes.is_empty());
    }
}
