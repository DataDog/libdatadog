// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A read-only view of a span that falls back to its enclosing chunk.

use crate::{Attributes, AttributesIter, ORIGIN_KEY, Span, SpanTags, TagValue, TraceChunk, Value};

/// Wraps a span together with its enclosing chunk.
///
/// Chunk attributes hold values common to every span in the chunk (e.g. peer tags). Attribute
/// and tag lookups fall back to the chunk whenever the span doesn't have its own value for a given
/// key, with [`Span::tag_str`] also resolving the chunk's promoted tags (e.g. `_dd.origin`).
pub struct ChunkSpanView<'a, C: TraceChunk> {
    pub span: &'a C::Span,
    pub chunk: &'a C,
}

impl<C: TraceChunk> Attributes for ChunkSpanView<'_, C> {
    type Text = C::Text;
    type Bytes = C::Bytes;
    type Values = C::Values;

    // Typed reads fall back per type, so a span attribute of the wrong type doesn't shadow a
    // matching chunk attribute.
    fn attribute_str(&self, key: &str) -> Option<&str> {
        self.span
            .attribute_str(key)
            .or_else(|| self.chunk.attribute_str(key))
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.span
            .attribute_f64(key)
            .or_else(|| self.chunk.attribute_f64(key))
    }

    fn attribute_i64(&self, key: &str) -> Option<i64> {
        self.span
            .attribute_i64(key)
            .or_else(|| self.chunk.attribute_i64(key))
    }
}

/// Yields the span attributes, then the chunk attributes whose key the span doesn't have.
impl<C> AttributesIter for ChunkSpanView<'_, C>
where
    C: TraceChunk + AttributesIter,
    C::Span: AttributesIter,
{
    fn attributes(&self) -> impl Iterator<Item = (&str, &Value<Self>)> {
        self.span.attributes().chain(
            self.chunk
                .attributes()
                .filter(|(k, _)| !self.span.has_attribute(k)),
        )
    }

    fn has_attribute(&self, key: &str) -> bool {
        self.span.has_attribute(key) || self.chunk.has_attribute(key)
    }
}

impl<C: TraceChunk> Span for ChunkSpanView<'_, C> {
    type SpanEvent = <C::Span as Span>::SpanEvent;
    type SpanLink = <C::Span as Span>::SpanLink;

    fn service(&self) -> &str {
        self.span.service()
    }

    fn resource(&self) -> &str {
        self.span.resource()
    }

    fn name(&self) -> &str {
        self.span.name()
    }

    fn typ(&self) -> &str {
        self.span.typ()
    }

    fn span_id(&self) -> u64 {
        self.span.span_id()
    }

    fn parent_id(&self) -> u64 {
        self.span.parent_id()
    }

    fn start(&self) -> i64 {
        self.span.start()
    }

    fn duration(&self) -> i64 {
        self.span.duration()
    }

    fn is_error(&self) -> bool {
        self.span.is_error()
    }

    // Chunks have no dedicated fields for these, but their attributes hold values common to
    // every span: the span's own field wins, then the chunk attribute fills in.
    fn span_kind(&self) -> Option<&str> {
        self.span
            .span_kind()
            .or_else(|| self.chunk.attribute_str(crate::SPAN_KIND_KEY))
    }

    fn env(&self) -> Option<&str> {
        self.span
            .env()
            .or_else(|| self.chunk.attribute_str(crate::ENV_KEY))
    }

    fn version(&self) -> Option<&str> {
        self.span
            .version()
            .or_else(|| self.chunk.attribute_str(crate::VERSION_KEY))
    }

    fn component(&self) -> Option<&str> {
        self.span
            .component()
            .or_else(|| self.chunk.attribute_str(crate::COMPONENT_KEY))
    }

    // Promoted span tags resolve through the getters above (which include the chunk fallback);
    // other keys go through the span's lookup, then the chunk's (e.g. its `_dd.origin` field).
    fn tag_str(&self, key: &str) -> Option<&str> {
        match key {
            crate::SPAN_KIND_KEY => self.span_kind(),
            crate::ENV_KEY => self.env(),
            crate::VERSION_KEY => self.version(),
            crate::COMPONENT_KEY => self.component(),
            _ => self.span.tag_str(key).or_else(|| self.chunk.tag_str(key)),
        }
    }

    fn span_events(&self) -> impl Iterator<Item = &Self::SpanEvent> {
        self.span.span_events()
    }

    fn span_links(&self) -> impl Iterator<Item = &Self::SpanLink> {
        self.span.span_links()
    }

    // Span-level markers are never inherited from the chunk.
    fn is_trace_root(&self) -> bool {
        self.span.is_trace_root()
    }

    fn has_top_level(&self) -> bool {
        self.span.has_top_level()
    }

    fn is_measured(&self) -> bool {
        self.span.is_measured()
    }

    fn is_partial_snapshot(&self) -> bool {
        self.span.is_partial_snapshot()
    }
}

/// Yields the span's tags, then the chunk-level ones (its attributes, and its origin as
/// `_dd.origin`) whose name the span doesn't have: the span's own values always win.
impl<C> SpanTags for ChunkSpanView<'_, C>
where
    C: TraceChunk + AttributesIter,
    C::Span: SpanTags,
{
    fn tags(&self) -> impl Iterator<Item = (&str, TagValue<'_, Self::Values>)> {
        let origin = Some(self.chunk.origin())
            .filter(|origin| !origin.is_empty())
            .map(|origin| (ORIGIN_KEY, TagValue::Str(origin)));
        let chunk_tags = self
            .chunk
            .attributes()
            .map(|(key, value)| (key, TagValue::Value(value)))
            .chain(origin)
            .filter(|(key, _)| !self.span.has_tag(key));
        self.span.tags().chain(chunk_tags)
    }

    fn has_tag(&self, name: &str) -> bool {
        self.tags().any(|(key, _)| key == name)
    }
}
