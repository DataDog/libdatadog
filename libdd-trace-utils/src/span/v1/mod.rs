// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;
use crate::span::{BytesData, SliceData, TraceData};
pub use thin_vec::ThinVec;

/// OpenTelemetry SpanKind values, encoded on the wire as a `uint32`.
/// Unset or unrecognized kinds are [`SpanKind::Unspecified`] (OTel `SPAN_KIND_UNSPECIFIED`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanKind {
    #[default]
    Unspecified = 0,
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

impl SpanKind {
    /// Parses a v0.4 `span.kind` meta value into a [`SpanKind`].
    /// Unrecognized values map to [`SpanKind::Unspecified`].
    pub fn from_meta(s: &str) -> Self {
        match s {
            "internal" => SpanKind::Internal,
            "server" => SpanKind::Server,
            "client" => SpanKind::Client,
            "producer" => SpanKind::Producer,
            "consumer" => SpanKind::Consumer,
            _ => SpanKind::Unspecified,
        }
    }

    /// Renders this [`SpanKind`] as the lowercase string used for the v0.4 `span.kind` meta value
    /// (empty for `Unspecified`).
    pub fn as_meta_str(&self) -> &'static str {
        match self {
            SpanKind::Unspecified => "",
            SpanKind::Internal => "internal",
            SpanKind::Server => "server",
            SpanKind::Client => "client",
            SpanKind::Producer => "producer",
            SpanKind::Consumer => "consumer",
        }
    }
}

impl From<u32> for SpanKind {
    /// OTEL SpanKind wire value → enum; unset/unknown → Unspecified.
    fn from(kind: u32) -> Self {
        match kind {
            1 => SpanKind::Internal,
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Unspecified,
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

// Implemented manually rather than derived: `VecMap`'s `PartialEq` is gated to
// test/test-utils (see its definition) to keep its allocation cost out of casual `==`, so the
// `KeyValue` variant compares via `slow_compare` instead of relying on that trait impl.
impl<T: TraceData> PartialEq for AttributeValue<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AttributeValue::String(a), AttributeValue::String(b)) => a == b,
            (AttributeValue::Float(a), AttributeValue::Float(b)) => a == b,
            (AttributeValue::Int(a), AttributeValue::Int(b)) => a == b,
            (AttributeValue::Bool(a), AttributeValue::Bool(b)) => a == b,
            (AttributeValue::Bytes(a), AttributeValue::Bytes(b)) => a == b,
            (AttributeValue::KeyValue(a), AttributeValue::KeyValue(b)) => a.slow_compare(b),
            (AttributeValue::List(a), AttributeValue::List(b)) => a == b,
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
/// use libdd_trace_utils::span::{TraceData, v1::Span};
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

type AttributeMap<T> = VecMap<<T as TraceData>::Text, AttributeValue<T>>;

/// Applies `f` to an attribute map, then to every map nested in its values. `f` runs first so a
/// dedup never visits the values of dropped duplicates.
fn visit_attr_map<T: TraceData>(
    map: &mut AttributeMap<T>,
    f: &mut impl FnMut(&mut AttributeMap<T>),
) {
    f(map);
    for v in map.values_mut() {
        v.visit_attr_maps(f);
    }
}

impl<T: TraceData> AttributeValue<T> {
    fn visit_attr_maps(&mut self, f: &mut impl FnMut(&mut AttributeMap<T>)) {
        match self {
            AttributeValue::KeyValue(map) => visit_attr_map(map, f),
            AttributeValue::List(list) => list.iter_mut().for_each(|v| v.visit_attr_maps(f)),
            _ => {}
        }
    }
}

impl<T: TraceData> Span<T> {
    fn visit_attr_maps(&mut self, f: &mut impl FnMut(&mut AttributeMap<T>)) {
        visit_attr_map(&mut self.attributes, f);
        for link in &mut self.span_links {
            visit_attr_map(&mut link.attributes, f);
        }
        for event in &mut self.span_events {
            visit_attr_map(&mut event.attributes, f);
        }
    }

    /// Dedup this span's attribute maps, including its links' and events'.
    pub fn dedup(&mut self) {
        self.visit_attr_maps(&mut VecMap::dedup);
    }
}

impl<T: TraceData> TraceChunk<T> {
    fn visit_attr_maps(&mut self, f: &mut impl FnMut(&mut AttributeMap<T>)) {
        visit_attr_map(&mut self.attributes, f);
        for span in &mut self.spans {
            span.visit_attr_maps(f);
        }
    }

    /// Dedup the chunk's attribute maps and those of every span it carries.
    pub fn dedup(&mut self) {
        self.visit_attr_maps(&mut VecMap::dedup);
    }
}

impl<T: TraceData> TracerPayload<T> {
    fn visit_attr_maps(&mut self, f: &mut impl FnMut(&mut AttributeMap<T>)) {
        visit_attr_map(&mut self.attributes, f);
        for chunk in &mut self.chunks {
            chunk.visit_attr_maps(f);
        }
    }

    /// Dedup every attribute map, so encoding takes the no-copy path of `defensive_dedup`.
    pub fn dedup(&mut self) {
        self.visit_attr_maps(&mut VecMap::dedup);
    }

    /// Flags every attribute map as deduped without scanning, for a payload decoded from an
    /// encoding of an already-deduped payload (see [`VecMap::mark_deduped`]).
    pub fn mark_deduped(&mut self) {
        self.visit_attr_maps(&mut VecMap::mark_deduped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_kind_default_is_unspecified() {
        assert_eq!(SpanKind::default(), SpanKind::Unspecified);
        assert_eq!(SpanKind::from(0), SpanKind::Unspecified);
        assert_eq!(SpanKind::from(1), SpanKind::Internal);
        assert_eq!(SpanKind::from(9), SpanKind::Unspecified);
    }

    fn nested_attrs() -> AttributeMap<BytesData> {
        let mut inner = VecMap::new();
        inner.insert("k".into(), AttributeValue::Int(1));
        inner.insert("k".into(), AttributeValue::Int(2));
        let mut map = VecMap::new();
        map.insert("m".into(), AttributeValue::KeyValue(inner));
        map
    }

    fn nested_is_deduped(map: &AttributeMap<BytesData>) -> bool {
        matches!(map.get("m"), Some(AttributeValue::KeyValue(m)) if m.is_deduped())
    }

    #[test]
    fn payload_dedup_reaches_nested_maps_and_keeps_flags_set() {
        let mut payload = TracerPayloadBytes {
            attributes: nested_attrs(),
            chunks: vec![TraceChunk {
                spans: vec![Span {
                    attributes: nested_attrs(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        payload.dedup();
        let span_attrs = &payload.chunks[0].spans[0].attributes;
        assert!(payload.attributes.is_deduped() && span_attrs.is_deduped());
        assert!(nested_is_deduped(&payload.attributes) && nested_is_deduped(span_attrs));
        let Some(AttributeValue::KeyValue(m)) = span_attrs.get("m") else {
            panic!("nested map missing");
        };
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("k"), Some(&AttributeValue::Int(2)));
    }

    #[test]
    fn payload_mark_deduped_flags_nested_maps_without_dropping_entries() {
        let mut payload = TracerPayloadBytes {
            attributes: nested_attrs(),
            ..Default::default()
        };
        payload.mark_deduped();
        assert!(payload.attributes.is_deduped() && nested_is_deduped(&payload.attributes));
        let Some(AttributeValue::KeyValue(m)) = payload.attributes.get("m") else {
            panic!("nested map missing");
        };
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn span_kind_from_meta() {
        assert_eq!(SpanKind::from_meta("server"), SpanKind::Server);
        assert_eq!(SpanKind::from_meta("client"), SpanKind::Client);
        assert_eq!(SpanKind::from_meta("producer"), SpanKind::Producer);
        assert_eq!(SpanKind::from_meta("consumer"), SpanKind::Consumer);
        assert_eq!(SpanKind::from_meta("internal"), SpanKind::Internal);
        assert_eq!(SpanKind::from_meta(""), SpanKind::Unspecified);
        assert_eq!(SpanKind::from_meta("anything-else"), SpanKind::Unspecified);
    }

    #[test]
    fn span_kind_repr_matches_otel_spec() {
        assert_eq!(SpanKind::Unspecified as u32, 0);
        assert_eq!(SpanKind::Internal as u32, 1);
        assert_eq!(SpanKind::Server as u32, 2);
        assert_eq!(SpanKind::Client as u32, 3);
        assert_eq!(SpanKind::Producer as u32, 4);
        assert_eq!(SpanKind::Consumer as u32, 5);
    }

    #[test]
    fn span_default_has_unspecified_kind() {
        let s = SpanBytes::default();
        assert_eq!(s.span_kind, SpanKind::Unspecified);
        assert!(!s.error);
        assert!(s.attributes.is_empty());
    }
}
