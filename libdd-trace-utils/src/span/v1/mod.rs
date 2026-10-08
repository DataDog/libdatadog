// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;
use crate::span::{BytesData, SliceData, TraceData};
pub use libdd_trace_model::AttributeValue;
use libdd_trace_model::{TraceText, Value};
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

    /// Renders this [`SpanKind`] as the lowercase string used for the v0.4 `span.kind` meta value.
    pub fn as_meta_str(&self) -> &'static str {
        match self {
            SpanKind::Internal => "internal",
            SpanKind::Server => "server",
            SpanKind::Client => "client",
            SpanKind::Producer => "producer",
            SpanKind::Consumer => "consumer",
        }
    }
}

impl From<u32> for SpanKind {
    /// OTEL SpanKind wire value → enum; unset/unknown → Internal (per OTEL spec).
    fn from(kind: u32) -> Self {
        match kind {
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Internal,
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

/// Implements [`libdd_trace_model::Attributes`] and [`libdd_trace_model::AttributesIter`] for a v1
/// type storing its attributes in an `attributes: VecMap<T::Text, AttributeValue<T>>` field.
macro_rules! impl_vec_map_attributes {
    ($ty:ident) => {
        impl<T: TraceData> libdd_trace_model::Attributes for $ty<T> {
            type Text = T::Text;
            type Bytes = T::Bytes;
            type Values = T;

            fn attribute_str(&self, key: &str) -> Option<&str> {
                self.attributes.get(key)?.as_str()
            }

            fn attribute_f64(&self, key: &str) -> Option<f64> {
                self.attributes.get(key)?.as_f64()
            }

            fn attribute_i64(&self, key: &str) -> Option<i64> {
                self.attributes.get(key)?.as_i64()
            }
        }

        impl<T: TraceData> libdd_trace_model::AttributesIter for $ty<T> {
            fn attributes(&self) -> impl Iterator<Item = (&str, &Value<Self>)> {
                self.attributes.iter_unique().map(|(k, v)| (k.as_str(), v))
            }

            fn has_attribute(&self, key: &str) -> bool {
                self.attributes.contains_key(key)
            }
        }
    };
}

impl_vec_map_attributes!(Span);
impl_vec_map_attributes!(SpanEvent);
impl_vec_map_attributes!(SpanLink);
impl_vec_map_attributes!(TraceChunk);

impl<T: TraceData> libdd_trace_model::Span for Span<T> {
    type SpanEvent = SpanEvent<T>;
    type SpanLink = SpanLink<T>;

    fn service(&self) -> &str {
        self.service.as_str()
    }

    fn resource(&self) -> &str {
        self.resource.as_str()
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn typ(&self) -> &str {
        self.r#type.as_str()
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn parent_id(&self) -> u64 {
        self.parent_id
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn is_error(&self) -> bool {
        self.error
    }

    fn span_kind(&self) -> Option<&str> {
        // `Internal` is the wire-level default and indistinguishable from "unset", so it's
        // treated as no value.
        (self.span_kind != SpanKind::Internal).then(|| self.span_kind.as_meta_str())
    }

    // `env`, `version` and `component` are promoted to dedicated fields rather than stored in
    // `attributes`; empty text is treated as unset.
    fn env(&self) -> Option<&str> {
        non_empty(&self.env)
    }

    fn version(&self) -> Option<&str> {
        non_empty(&self.version)
    }

    fn component(&self) -> Option<&str> {
        non_empty(&self.component)
    }

    fn span_events(&self) -> impl Iterator<Item = &Self::SpanEvent> {
        self.span_events.iter()
    }

    fn span_links(&self) -> impl Iterator<Item = &Self::SpanLink> {
        self.span_links.iter()
    }
}

/// Returns `text` as a `&str`, or `None` if it is empty.
fn non_empty<T: TraceText>(text: &T) -> Option<&str> {
    Some(text.as_str()).filter(|s| !s.is_empty())
}

impl<T: TraceData> libdd_trace_model::SpanTags for Span<T> {}

impl<T: TraceData> libdd_trace_model::SpanEvent for SpanEvent<T> {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn time_unix_nano(&self) -> u64 {
        self.time_unix_nano
    }
}

impl<T: TraceData> libdd_trace_model::TraceChunk for TraceChunk<T> {
    type Span = Span<T>;

    fn trace_id(&self) -> u128 {
        u128::from_be_bytes(self.trace_id)
    }

    fn origin(&self) -> &str {
        self.origin.as_str()
    }

    fn spans(&self) -> impl Iterator<Item = &Self::Span> {
        self.spans.iter()
    }

    fn priority(&self) -> Option<i32> {
        self.priority
    }

    fn sampling_mechanism(&self) -> Option<u32> {
        self.sampling_mechanism
    }

    fn dropped_trace(&self) -> bool {
        self.dropped_trace
    }
}

impl<T: TraceData> libdd_trace_model::SpanLink for SpanLink<T> {
    fn trace_id(&self) -> u128 {
        u128::from_be_bytes(self.trace_id)
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn tracestate(&self) -> &str {
        self.tracestate.as_str()
    }

    fn flags(&self) -> u32 {
        self.flags
    }
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
    use libdd_tinybytes::BytesString;
    use libdd_trace_model::{
        Attributes as _, ChunkSpanView, Span as _, SpanEvent as _, SpanLink as _, SpanTags as _,
        TraceChunk as _,
    };

    #[test]
    fn model_traits_expose_v1_fields() {
        let mut span = SpanBytes {
            span_id: 1,
            ..Default::default()
        };
        span.attributes.insert(
            BytesString::from_static("big"),
            AttributeValue::Int(i64::MAX),
        );
        span.span_links.push(SpanLinkBytes {
            trace_id: 7u128.to_be_bytes(),
            span_id: 2,
            tracestate: BytesString::from_static("dd=s:1"),
            flags: 1,
            ..Default::default()
        });
        span.span_events.push(SpanEventBytes {
            time_unix_nano: 3,
            name: BytesString::from_static("evt"),
            ..Default::default()
        });

        // Ints above 2^53 stay exact, unlike through `attribute_f64`.
        assert_eq!(span.attribute_i64("big"), Some(i64::MAX));
        let link = span.span_links().next().unwrap();
        assert_eq!(
            (
                link.trace_id(),
                link.span_id(),
                link.tracestate(),
                link.flags()
            ),
            (7, 2, "dd=s:1", 1)
        );
        let event = span.span_events().next().unwrap();
        assert_eq!((event.name(), event.time_unix_nano()), ("evt", 3));

        let chunk = TraceChunkBytes {
            spans: vec![span, SpanBytes::default()],
            ..Default::default()
        };
        assert_eq!(
            (
                chunk.priority(),
                chunk.sampling_mechanism(),
                chunk.dropped_trace()
            ),
            (None, None, false)
        );
        assert_eq!(chunk.tag_str("_dd.origin"), None);
        let chunk = TraceChunkBytes {
            dropped_trace: true,
            origin: BytesString::from_static("synthetics"),
            priority: Some(2),
            sampling_mechanism: Some(4),
            ..chunk
        };
        assert_eq!(
            (
                chunk.priority(),
                chunk.sampling_mechanism(),
                chunk.dropped_trace()
            ),
            (Some(2), Some(4), true)
        );
        assert_eq!(chunk.origin(), "synthetics");
        assert_eq!(chunk.tag_str("_dd.origin"), Some("synthetics"));
        assert_eq!(chunk.spans().filter(|s| s.span_id() == 1).count(), 1);
    }

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

    #[test]
    fn model_span_kind_reads_dedicated_field() {
        let mut span = SpanBytes::default();
        // Internal is indistinguishable from unset.
        assert_eq!(span.span_kind(), None);
        span.span_kind = SpanKind::Client;
        assert_eq!(span.span_kind(), Some("client"));
        assert_eq!(span.tag_str("span.kind"), Some("client"));
    }

    #[test]
    fn promoted_span_tags_are_read_from_fields() {
        let span = SpanBytes {
            env: BytesString::from_static("prod"),
            version: BytesString::from_static("1.2"),
            component: BytesString::from_static("http"),
            ..Default::default()
        };
        assert_eq!(
            (span.env(), span.version(), span.component()),
            (Some("prod"), Some("1.2"), Some("http"))
        );
        assert_eq!(span.tag_str("env"), Some("prod"));
        // The attribute getters don't see promoted tags.
        assert_eq!(span.attribute_str("env"), None);
        // Empty text is unset.
        assert_eq!(SpanBytes::default().env(), None);
    }

    #[test]
    fn chunk_span_view_tags_layer_span_over_chunk() {
        let s = |v: &'static str| AttributeValue::String(BytesString::from_static(v));
        let mut span = SpanBytes {
            env: BytesString::from_static("staging"),
            ..Default::default()
        };
        span.attributes
            .insert(BytesString::from_static("peer.service"), s("span-db"));
        let mut chunk = TraceChunkBytes {
            spans: vec![span],
            origin: BytesString::from_static("synthetics"),
            ..Default::default()
        };
        for (k, v) in [
            ("env", "prod"),
            ("peer.service", "chunk-db"),
            ("team", "core"),
        ] {
            chunk.attributes.insert(BytesString::from_static(k), s(v));
        }
        let view = ChunkSpanView {
            span: &chunk.spans[0],
            chunk: &chunk,
        };
        let mut tags: Vec<(&str, &str)> =
            view.tags().map(|(k, v)| (k, v.as_str().unwrap())).collect();
        tags.sort_unstable();
        assert_eq!(
            tags,
            [
                ("_dd.origin", "synthetics"),
                ("env", "staging"),
                ("peer.service", "span-db"),
                ("team", "core"),
            ]
        );
        assert_eq!(view.tag_str("team"), Some("core"));
        assert_eq!(view.tag_str("_dd.origin"), Some("synthetics"));
        assert!(view.has_tag("_dd.origin"));
        assert!(!view.has_tag("version"));
    }

    #[test]
    fn model_attributes_coerce_ints_to_f64() {
        let mut span = SpanBytes::default();
        span.attributes
            .insert(BytesString::from("_top_level"), AttributeValue::Int(1));
        span.attributes.insert(
            BytesString::from("_dd.partial_version"),
            AttributeValue::Float(0.0),
        );
        assert_eq!(span.attribute_f64("_top_level"), Some(1.0));
        assert_eq!(span.attribute_str("_top_level"), None);
        assert!(span.has_top_level());
        assert!(span.is_partial_snapshot());
        assert!(!span.is_measured());
    }
}
