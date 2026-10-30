// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;
use crate::span::{BytesData, SliceData, TraceData};
pub use libdd_trace_model::AttributeValue;
use libdd_trace_model::TraceText;
pub use thin_vec::ThinVec;

pub mod span_pool;

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

/// Implements [`libdd_trace_model::Attributes`], [`libdd_trace_model::AttributesIter`] and
/// [`libdd_trace_model::AttributesMut`] for a v1 type storing its attributes in an
/// `attributes: VecMap<T::Text, AttributeValue<T>>` field.
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
            fn attributes(
                &self,
            ) -> impl Iterator<Item = (&str, libdd_trace_model::AttributeRef<'_, T>)> {
                self.attributes
                    .iter_unique()
                    .map(|(k, v)| (k.as_str(), v.into()))
            }

            fn has_attribute(&self, key: &str) -> bool {
                self.attributes.contains_key(key)
            }
        }

        impl<T: TraceData> libdd_trace_model::AttributesMut for $ty<T> {
            fn retain_attributes(
                &mut self,
                f: impl FnMut(&str, &mut libdd_trace_model::Value<Self>) -> bool,
            ) {
                libdd_trace_model::ValueMapMut::retain(&mut self.attributes, f);
            }

            fn attribute_mut(&mut self, key: &str) -> Option<&mut libdd_trace_model::Value<Self>> {
                self.attributes.get_mut(key)
            }

            fn set_attribute(
                &mut self,
                key: impl Into<Self::Text>,
                value: libdd_trace_model::Value<Self>,
            ) {
                self.attributes.insert_or_replace(key.into(), value);
            }

            fn remove_attribute(&mut self, key: &str) -> Option<libdd_trace_model::Value<Self>> {
                self.attributes.remove(key)
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

impl<T: TraceData> libdd_trace_model::SpanMut for Span<T> {
    fn set_service(&mut self, value: impl Into<Self::Text>) {
        self.service = value.into();
    }

    fn set_resource(&mut self, value: impl Into<Self::Text>) {
        self.resource = value.into();
    }

    fn set_name(&mut self, value: impl Into<Self::Text>) {
        self.name = value.into();
    }

    fn set_type(&mut self, value: impl Into<Self::Text>) {
        self.r#type = value.into();
    }

    fn set_parent_id(&mut self, value: u64) {
        self.parent_id = value;
    }

    fn set_start(&mut self, value: i64) {
        self.start = value;
    }

    fn set_duration(&mut self, value: i64) {
        self.duration = value;
    }

    fn set_error(&mut self, value: bool) {
        self.error = value;
    }

    fn set_span_kind(&mut self, value: &str) {
        self.span_kind = SpanKind::from_meta(value);
    }

    fn set_env(&mut self, value: impl Into<Self::Text>) {
        self.env = value.into();
    }

    fn set_version(&mut self, value: impl Into<Self::Text>) {
        self.version = value.into();
    }

    fn set_component(&mut self, value: impl Into<Self::Text>) {
        self.component = value.into();
    }

    fn span_events_mut(&mut self) -> impl Iterator<Item = &mut Self::SpanEvent> {
        self.span_events.iter_mut()
    }

    fn span_links_mut(&mut self) -> impl Iterator<Item = &mut Self::SpanLink> {
        self.span_links.iter_mut()
    }
}

/// Returns `text` as a `&str`, or `None` if it is empty.
fn non_empty<T: TraceText>(text: &T) -> Option<&str> {
    Some(text.as_str()).filter(|s| !s.is_empty())
}

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

impl<T: TraceData> libdd_trace_model::TraceChunkMut for TraceChunk<T> {
    fn set_origin(&mut self, value: impl Into<Self::Text>) {
        self.origin = value.into();
    }

    fn set_priority(&mut self, priority: i32) {
        self.priority = Some(priority);
    }

    fn set_sampling_mechanism(&mut self, mechanism: u32) {
        self.sampling_mechanism = Some(mechanism);
    }

    fn set_dropped_trace(&mut self, value: bool) {
        self.dropped_trace = value;
    }

    fn spans_mut(&mut self) -> impl Iterator<Item = &mut Self::Span> {
        self.spans.iter_mut()
    }

    fn retain_spans(&mut self, f: impl FnMut(&mut Self::Span) -> bool) {
        self.spans.retain_mut(f);
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
        AttributeRef, Attributes as _, AttributesIter as _, AttributesMut as _, ChunkSpanView,
        Span as _, SpanEvent as _, SpanLink as _, SpanMut as _, TraceChunk as _,
        TraceChunkMut as _,
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
        assert_eq!(chunk.origin(), "");
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
        // The chunk origin isn't an attribute.
        assert_eq!(chunk.attribute_str("_dd.origin"), None);
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
        assert_eq!(span.compat().attribute_str("span.kind"), Some("client"));
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
        // The attribute getters don't see promoted tags, unless through the compat view.
        assert_eq!(span.attribute_str("env"), None);
        assert_eq!(span.compat().attribute_str("env"), Some("prod"));
        assert!(span.compat().has_attribute("env"));
        // Promoted tags are strings.
        assert_eq!(span.compat().attribute_f64("env"), None);
        // Empty text is unset.
        assert_eq!(SpanBytes::default().env(), None);
    }

    #[test]
    fn model_mut_traits_write_v1_fields() {
        let mut span = SpanBytes {
            span_id: 1,
            ..Default::default()
        };
        span.span_links.push(SpanLinkBytes::default());
        span.span_events.push(SpanEventBytes::default());
        for link in span.span_links_mut() {
            link.set_attribute(BytesString::from_static("k"), AttributeValue::Bool(true));
        }
        for event in span.span_events_mut() {
            event.set_attribute(BytesString::from_static("k"), AttributeValue::Int(1));
        }
        assert_eq!(
            span.span_links[0].attributes.get("k"),
            Some(&AttributeValue::Bool(true))
        );
        assert_eq!(span.span_events[0].attribute_i64("k"), Some(1));

        span.set_service(BytesString::from_static("svc"));
        span.set_resource(BytesString::from_static("GET /"));
        span.set_name(BytesString::from_static("http.request"));
        span.set_type(BytesString::from_static("web"));
        span.set_parent_id(9);
        span.set_start(10);
        span.set_duration(20);
        span.set_env(BytesString::from_static("prod"));
        span.set_version(BytesString::from_static("1.2"));
        span.set_component(BytesString::from_static("http"));
        assert_eq!(
            (span.service(), span.resource(), span.name(), span.typ()),
            ("svc", "GET /", "http.request", "web")
        );
        assert_eq!(
            (span.parent_id(), span.start(), span.duration()),
            (9, 10, 20)
        );
        assert_eq!(
            (span.env(), span.version(), span.component()),
            (Some("prod"), Some("1.2"), Some("http"))
        );
        // The empty string unsets a text field.
        span.set_env(BytesString::from_static(""));
        assert_eq!(span.env(), None);

        span.set_attribute(BytesString::from_static("big"), AttributeValue::Int(1));
        if let Some(value) = span.attribute_mut("big") {
            *value = AttributeValue::Int(i64::MAX);
        }
        assert_eq!(span.attribute_i64("big"), Some(i64::MAX));
        assert_eq!(
            span.remove_attribute("big"),
            Some(AttributeValue::Int(i64::MAX))
        );
        assert_eq!(span.remove_attribute("big"), None);

        let mut chunk = TraceChunkBytes {
            spans: vec![span, SpanBytes::default()],
            ..Default::default()
        };
        chunk.set_origin(BytesString::from_static("synthetics"));
        chunk.set_priority(2);
        chunk.set_sampling_mechanism(4);
        chunk.set_dropped_trace(true);
        assert_eq!(
            (
                chunk.origin(),
                chunk.priority(),
                chunk.sampling_mechanism(),
                chunk.dropped_trace()
            ),
            ("synthetics", Some(2), Some(4), true)
        );
        // Chunk setters don't touch the attributes.
        assert!(chunk.attributes.is_empty());

        for span in chunk.spans_mut() {
            span.set_error(true);
        }
        chunk.retain_spans(|span| span.span_id() == 1);
        assert_eq!(chunk.spans.len(), 1);
        assert!(chunk.spans[0].is_error());
    }

    #[test]
    fn retain_attributes_sees_each_key_once() {
        let mut span = SpanBytes::default();
        // A shadowed duplicate, as a decoder may produce.
        span.attributes
            .insert(BytesString::from_static("k"), AttributeValue::Int(1));
        span.attributes
            .insert(BytesString::from_static("k"), AttributeValue::Int(2));
        span.attributes
            .insert(BytesString::from_static("drop"), AttributeValue::Int(3));
        let mut seen = Vec::new();
        span.retain_attributes(|key, value| {
            seen.push((key.to_owned(), value.as_i64()));
            if let AttributeValue::Int(i) = value {
                *i += 10;
            }
            key != "drop"
        });
        seen.sort_unstable();
        assert_eq!(
            seen,
            [("drop".to_owned(), Some(3)), ("k".to_owned(), Some(2))]
        );
        assert_eq!(span.attribute_i64("k"), Some(12));
        assert!(!span.has_attribute("drop"));
    }

    #[test]
    fn set_span_kind_maps_to_enum() {
        let mut span = SpanBytes::default();
        span.set_span_kind("client");
        assert_eq!(span.span_kind, SpanKind::Client);
        assert_eq!(span.span_kind(), Some("client"));
        // Kinds outside the OTel ones read as unset.
        span.set_span_kind("custom");
        assert_eq!(span.span_kind(), None);
    }

    #[test]
    fn plain_attribute_writes_skip_promoted_fields() {
        let s = |v: &'static str| AttributeValue::String(BytesString::from_static(v));
        let mut span = SpanBytes {
            env: BytesString::from_static("prod"),
            ..Default::default()
        };
        span.set_attribute(BytesString::from_static("env"), s("staging"));
        assert_eq!(span.env(), Some("prod"));
        assert_eq!(span.attribute_str("env"), Some("staging"));
        assert_eq!(span.remove_attribute("env"), Some(s("staging")));
        assert_eq!(span.env(), Some("prod"));
    }

    #[test]
    fn compat_mut_redirects_promoted_span_tags_to_fields() {
        let s = |v: &'static str| AttributeValue::String(BytesString::from_static(v));
        let mut span = SpanBytes::default();
        let compat = span.compat_mut();
        compat.set_attribute(BytesString::from_static("span.kind"), s("server"));
        compat.set_attribute(BytesString::from_static("env"), s("prod"));
        compat.set_attribute(BytesString::from_static("version"), s("1.2"));
        compat.set_attribute(BytesString::from_static("component"), s("http"));
        // Values the field can't hold are dropped rather than stored as attributes.
        compat.set_attribute(BytesString::from_static("env"), AttributeValue::Int(1));
        compat.set_attribute(
            BytesString::from_static("span.kind"),
            AttributeValue::Bool(true),
        );
        // Other keys are plain attributes.
        compat.set_attribute(BytesString::from_static("peer.service"), s("db"));
        assert_eq!(compat.attribute_str("env"), Some("prod"));
        assert_eq!(span.attributes.len(), 1);
        assert_eq!(span.attribute_str("peer.service"), Some("db"));
        assert_eq!(
            (
                span.span_kind,
                span.env.as_str(),
                span.version.as_str(),
                span.component.as_str()
            ),
            (SpanKind::Server, "prod", "1.2", "http")
        );
        // The plain attribute getters still don't see promoted tags.
        assert_eq!(span.attribute_str("env"), None);

        let compat = span.compat_mut();
        assert_eq!(compat.remove_attribute("span.kind"), Some(s("server")));
        assert_eq!(compat.remove_attribute("env"), Some(s("prod")));
        assert_eq!(compat.remove_attribute("env"), None);
        assert_eq!(compat.remove_attribute("peer.service"), Some(s("db")));
        assert_eq!((span.span_kind(), span.env()), (None, None));
        assert_eq!(span.version(), Some("1.2"));
        assert!(span.attributes.is_empty());

        let compat = span.compat_mut();
        assert_eq!(compat.remove_attribute("version"), Some(s("1.2")));
        assert_eq!(compat.remove_attribute("component"), Some(s("http")));
        assert_eq!((span.version(), span.component()), (None, None));
    }

    #[test]
    fn compat_mut_writes_drop_hidden_attributes_of_promoted_tags() {
        let s = |v: &'static str| AttributeValue::String(BytesString::from_static(v));
        // Stale attributes, e.g. from a tracer whose tags weren't promoted.
        let mut span = SpanBytes::default();
        for key in ["env", "version", "component"] {
            span.attributes
                .insert(BytesString::from_static(key), s("stale"));
        }

        let compat = span.compat_mut();
        compat.set_attribute(BytesString::from_static("env"), s("prod"));
        // The field was unset: compat reads ignore the hidden attribute, so nothing is returned.
        assert_eq!(compat.remove_attribute("version"), None);
        // A value the field can't hold is dropped, and leaves the hidden attribute alone.
        compat.set_attribute(
            BytesString::from_static("component"),
            AttributeValue::Int(1),
        );
        assert_eq!(span.env(), Some("prod"));
        assert!(!span.has_attribute("env"));
        assert!(!span.has_attribute("version"));
        assert_eq!(span.attribute_str("component"), Some("stale"));
    }

    #[test]
    fn compat_mut_hides_attributes_named_like_promoted_tags() {
        let s = |v: &'static str| AttributeValue::String(BytesString::from_static(v));
        let mut span = SpanBytes {
            env: BytesString::from_static("prod"),
            ..Default::default()
        };
        // A stale attribute, e.g. from a tracer that didn't promote it.
        span.attributes
            .insert(BytesString::from_static("env"), s("stale"));
        span.attributes
            .insert(BytesString::from_static("team"), s("core"));

        let compat = span.compat_mut();
        assert!(compat.attribute_mut("env").is_none());
        let mut seen = Vec::new();
        compat.retain_attributes(|key, _| {
            seen.push(key.to_owned());
            false
        });
        assert_eq!(seen, ["team"]);
        // The hidden attribute is kept untouched.
        assert_eq!(span.attribute_str("env"), Some("stale"));
        assert!(!span.has_attribute("team"));
    }

    #[test]
    fn compat_chunk_span_view_layers_span_over_chunk() {
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
            ("version", "1.2"),
            ("peer.service", "chunk-db"),
            ("team", "core"),
        ] {
            chunk.attributes.insert(BytesString::from_static(k), s(v));
        }
        let view = ChunkSpanView {
            span: &chunk.spans[0],
            chunk: &chunk,
        };
        // Without compat, chunk attributes named like promoted tags are only seen by the getters.
        assert_eq!(view.attribute_str("env"), None);
        assert_eq!(view.attribute_str("version"), None);
        assert!(!view.has_attribute("version"));
        assert_eq!((view.env(), view.version()), (Some("staging"), Some("1.2")));
        assert_eq!(view.attribute_str("team"), Some("core"));
        let compat = view.compat();
        let mut attributes: Vec<(&str, &str)> = compat
            .attributes()
            .map(|(k, v)| (k, v.as_str().unwrap()))
            .collect();
        attributes.sort_unstable();
        // Each key once: the span's `env` field shadows the chunk's `env` attribute.
        assert_eq!(
            attributes,
            [
                ("env", "staging"),
                ("peer.service", "span-db"),
                ("team", "core"),
                ("version", "1.2"),
            ]
        );
        assert_eq!(compat.attribute_str("team"), Some("core"));
        assert_eq!(compat.attribute_str("env"), Some("staging"));
        assert!(!compat.has_attribute("component"));
        // Promoted chunk tags get no compatibility.
        assert_eq!(compat.attribute_str("_dd.origin"), None);
        assert!(!compat.has_attribute("_dd.origin"));
    }

    #[test]
    fn attributes_iter_borrows_values() {
        let mut span = SpanBytes::default();
        span.attributes
            .insert(BytesString::from_static("n"), AttributeValue::Int(3));
        assert_eq!(
            span.attributes().collect::<Vec<_>>(),
            [("n", AttributeRef::Int(3))]
        );
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
}
