// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A read-only view of a span that falls back to its enclosing chunk.

use crate::{AttributeRef, Attributes, AttributesIter, Span, TraceChunk, is_promoted};

/// Wraps a span together with its enclosing chunk.
///
/// Chunk attributes hold values common to every span in the chunk (e.g. peer tags). Attribute
/// lookups fall back to the chunk whenever the span doesn't have its own value for a given key,
/// except for the promoted span tags (see [`crate::PROMOTED_SPAN_TAGS`]): like on a span, the
/// attribute getters don't see those, and the dedicated getters (e.g. [`Span::env`]) fall back to
/// the chunk attribute instead. Chunk-level fields (e.g. [`TraceChunk::origin`]) are read from
/// [`ChunkSpanView::chunk`].
pub struct ChunkSpanView<'a, C: TraceChunk> {
    pub span: &'a C::Span,
    pub chunk: &'a C,
}

impl<C: TraceChunk> ChunkSpanView<'_, C> {
    /// The chunk to fall back to for `key`, `None` for the promoted span tags.
    fn chunk_fallback(&self, key: &str) -> Option<&C> {
        (!is_promoted(key)).then_some(self.chunk)
    }
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
            .or_else(|| self.chunk_fallback(key)?.attribute_str(key))
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.span
            .attribute_f64(key)
            .or_else(|| self.chunk_fallback(key)?.attribute_f64(key))
    }

    fn attribute_i64(&self, key: &str) -> Option<i64> {
        self.span
            .attribute_i64(key)
            .or_else(|| self.chunk_fallback(key)?.attribute_i64(key))
    }
}

/// Yields the span attributes, then the chunk attributes whose key the span doesn't have (promoted
/// span tags excepted).
impl<C> AttributesIter for ChunkSpanView<'_, C>
where
    C: TraceChunk + AttributesIter,
    C::Span: AttributesIter,
{
    fn attributes(&self) -> impl Iterator<Item = (&str, AttributeRef<'_, Self::Values>)> {
        self.span.attributes().chain(
            self.chunk
                .attributes()
                .filter(|(k, _)| !is_promoted(k) && !self.span.has_attribute(k)),
        )
    }

    fn has_attribute(&self, key: &str) -> bool {
        self.span.has_attribute(key)
            || self
                .chunk_fallback(key)
                .is_some_and(|chunk| chunk.has_attribute(key))
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
