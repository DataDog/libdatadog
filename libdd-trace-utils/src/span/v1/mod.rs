// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;
use crate::span::{BytesData, SliceData, TraceData};
pub use libdd_trace_model::AttributeValue;
use libdd_trace_model::ValueTypes;
pub use thin_vec::ThinVec;

pub mod span_pool;

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

impl<T: TraceData> Span<T> {
    /// Deduplicates every attribute map on this span (its own `attributes`, and each span
    /// link's and span event's `attributes`), keeping the last occurrence of each key.
    pub fn dedup(&mut self) {
        self.attributes.dedup();
        for link in &mut self.span_links {
            link.attributes.dedup();
        }
        for event in &mut self.span_events {
            event.attributes.dedup();
        }
    }
}

impl<T: TraceData> TraceChunk<T> {
    /// Deduplicates this chunk's own `attributes`, plus every span's attribute maps.
    pub fn dedup(&mut self) {
        self.attributes.dedup();
        for span in &mut self.spans {
            span.dedup();
        }
    }
}

type AttributeMap<T> = VecMap<<T as ValueTypes>::Text, AttributeValue<T>>;

/// Applies `f` to the maps nested in `map`'s values, recursively. Call it before `f(map)`:
/// `iter_mut` clears `map`'s dedup flag.
fn for_each_nested_map<T: TraceData>(map: &mut AttributeMap<T>, f: fn(&mut AttributeMap<T>)) {
    for (_, v) in map.iter_mut() {
        for_each_map_in_value(v, f);
    }
}

fn for_each_map_in_value<T: TraceData>(v: &mut AttributeValue<T>, f: fn(&mut AttributeMap<T>)) {
    match v {
        AttributeValue::KeyValueList(map) => {
            for_each_nested_map(map, f);
            f(map);
        }
        AttributeValue::Array(items) => items.iter_mut().for_each(|v| for_each_map_in_value(v, f)),
        _ => {}
    }
}

impl<T: TraceData> TracerPayload<T> {
    /// Applies `f` to every attribute map of the payload, including the `KeyValueList` maps nested
    /// in values, which [`Span::dedup`] leaves to `defensive_dedup`'s on-the-fly copy.
    fn for_each_attr_map(&mut self, f: fn(&mut AttributeMap<T>)) {
        let apply = |map: &mut AttributeMap<T>| {
            for_each_nested_map(map, f);
            f(map);
        };
        apply(&mut self.attributes);
        for chunk in &mut self.chunks {
            apply(&mut chunk.attributes);
            for span in &mut chunk.spans {
                apply(&mut span.attributes);
                for link in &mut span.span_links {
                    apply(&mut link.attributes);
                }
                for event in &mut span.span_events {
                    apply(&mut event.attributes);
                }
            }
        }
    }

    /// Dedups every attribute map of the payload, nested ones included.
    pub fn dedup(&mut self) {
        self.for_each_attr_map(VecMap::dedup);
    }

    /// Flags every attribute map as deduped without scanning, for a payload decoded from an
    /// encoding of an already-deduped payload (see [`VecMap::mark_deduped`]).
    pub fn mark_deduped(&mut self) {
        self.for_each_attr_map(VecMap::mark_deduped);
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

    #[test]
    fn span_kind_default_is_unspecified() {
        assert_eq!(SpanKind::default(), SpanKind::Unspecified);
        assert_eq!(SpanKind::from(0), SpanKind::Unspecified);
        assert_eq!(SpanKind::from(1), SpanKind::Internal);
        assert_eq!(SpanKind::from(9), SpanKind::Unspecified);
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

    fn key(s: &str) -> libdd_tinybytes::BytesString {
        libdd_tinybytes::BytesString::from_slice(s.as_bytes()).unwrap()
    }

    #[test]
    fn span_dedup_removes_duplicate_keys_everywhere() {
        let mut span = SpanBytes::default();
        span.attributes.insert(key("a"), AttributeValue::Int(1));
        span.attributes.insert(key("a"), AttributeValue::Int(2));
        span.span_links.push(SpanLinkBytes::default());
        span.span_links[0]
            .attributes
            .insert(key("b"), AttributeValue::Int(1));
        span.span_links[0]
            .attributes
            .insert(key("b"), AttributeValue::Int(2));
        span.span_events.push(SpanEventBytes::default());
        span.span_events[0]
            .attributes
            .insert(key("c"), AttributeValue::Int(1));
        span.span_events[0]
            .attributes
            .insert(key("c"), AttributeValue::Int(2));

        span.dedup();

        assert_eq!(
            span.attributes.get(&key("a")),
            Some(&AttributeValue::Int(2))
        );
        assert_eq!(
            span.span_links[0].attributes.get(&key("b")),
            Some(&AttributeValue::Int(2))
        );
        assert_eq!(
            span.span_events[0].attributes.get(&key("c")),
            Some(&AttributeValue::Int(2))
        );
    }

    #[test]
    fn trace_chunk_dedup_dedups_chunk_and_span_attributes() {
        let mut chunk = TraceChunkBytes::default();
        chunk.attributes.insert(key("x"), AttributeValue::Int(1));
        chunk.attributes.insert(key("x"), AttributeValue::Int(2));
        let mut span = SpanBytes::default();
        span.attributes.insert(key("y"), AttributeValue::Int(1));
        span.attributes.insert(key("y"), AttributeValue::Int(2));
        chunk.spans.push(span);

        chunk.dedup();

        assert_eq!(
            chunk.attributes.get(&key("x")),
            Some(&AttributeValue::Int(2))
        );
        assert_eq!(
            chunk.spans[0].attributes.get(&key("y")),
            Some(&AttributeValue::Int(2))
        );
    }

    fn nested_attrs() -> AttributeMap<BytesData> {
        let mut inner = VecMap::new();
        inner.insert(key("k"), AttributeValue::Int(1));
        inner.insert(key("k"), AttributeValue::Int(2));
        let mut map = VecMap::new();
        map.insert(
            key("m"),
            AttributeValueBytes::Array(vec![AttributeValueBytes::KeyValueList(inner)]),
        );
        map
    }

    fn nested_map(map: &AttributeMap<BytesData>) -> &AttributeMap<BytesData> {
        match map.get(&key("m")) {
            Some(AttributeValue::Array(items)) => match &items[0] {
                AttributeValue::KeyValueList(m) => m,
                _ => panic!("nested map missing"),
            },
            _ => panic!("nested array missing"),
        }
    }

    #[test]
    fn tracer_payload_dedup_reaches_nested_maps() {
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
        let m = nested_map(span_attrs);
        assert!(m.is_deduped() && nested_map(&payload.attributes).is_deduped());
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&key("k")), Some(&AttributeValue::Int(2)));
    }

    #[test]
    fn tracer_payload_mark_deduped_flags_nested_maps_without_dropping_entries() {
        let mut payload = TracerPayloadBytes {
            attributes: nested_attrs(),
            ..Default::default()
        };
        payload.mark_deduped();
        let m = nested_map(&payload.attributes);
        assert!(payload.attributes.is_deduped() && m.is_deduped());
        assert_eq!(m.len(), 2);
    }
}
