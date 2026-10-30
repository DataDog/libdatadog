// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A trace chunk view for span formats without chunk-level storage (v0.4 and protobuf spans).

use crate::span::{TraceData, trace_utils::get_root_span_index, v04};
use libdd_trace_model::{
    AttributeRef, Attributes, AttributesIter, DECISION_MAKER_KEY, ORIGIN_KEY,
    SAMPLING_PRIORITY_KEY, Span, TraceChunk, f64_to_exact_i64,
};
use libdd_trace_protobuf::pb;

/// A span that carries its own trace id, as v0.4 and protobuf spans do.
pub trait SpanWithTraceId: Span {
    /// The 128-bit trace id of the span, `0` if unknown.
    fn trace_id(&self) -> u128;
}

impl<T: TraceData> SpanWithTraceId for v04::Span<T> {
    fn trace_id(&self) -> u128 {
        self.trace_id
    }
}

/// Only the low 64 bits: protobuf spans carry the high bits in the `_dd.p.tid` tag.
impl SpanWithTraceId for pb::Span {
    fn trace_id(&self) -> u128 {
        u128::from(self.trace_id)
    }
}

/// The spans of one trace, in a format without chunk-level storage, viewed as a trace chunk.
///
/// The chunk fields are read from the root span's tags, the way the agent derives them (e.g. the
/// origin from the root's `_dd.origin` tag), and the chunk has no attributes. The root span is
/// found once, on construction. A single span can be viewed as a one-span chunk with
/// `RootSpanChunk::new(std::slice::from_ref(span))`.
pub struct RootSpanChunk<'a, S> {
    spans: &'a [S],
    root: Option<&'a S>,
}

impl<'a, S: SpanWithTraceId> RootSpanChunk<'a, S> {
    pub fn new(spans: &'a [S]) -> Self {
        let root = get_root_span_index(spans).ok().and_then(|i| spans.get(i));
        Self { spans, root }
    }
}

/// These chunks have no attributes.
impl<S: SpanWithTraceId> Attributes for RootSpanChunk<'_, S> {
    type Text = S::Text;
    type Bytes = S::Bytes;
    type Values = S::Values;

    fn attribute_str(&self, _key: &str) -> Option<&str> {
        None
    }

    fn attribute_f64(&self, _key: &str) -> Option<f64> {
        None
    }
}

impl<S: SpanWithTraceId> AttributesIter for RootSpanChunk<'_, S> {
    fn attributes(&self) -> impl Iterator<Item = (&str, AttributeRef<'_, S::Values>)> {
        core::iter::empty()
    }
}

impl<S: SpanWithTraceId> TraceChunk for RootSpanChunk<'_, S> {
    type Span = S;

    fn trace_id(&self) -> u128 {
        self.root.map_or(0, SpanWithTraceId::trace_id)
    }

    fn origin(&self) -> &str {
        self.root
            .and_then(|root| root.attribute_str(ORIGIN_KEY))
            .unwrap_or_default()
    }

    fn spans(&self) -> impl Iterator<Item = &Self::Span> {
        self.spans.iter()
    }

    fn priority(&self) -> Option<i32> {
        let priority = self.root?.attribute_f64(SAMPLING_PRIORITY_KEY)?;
        i32::try_from(f64_to_exact_i64(priority)?).ok()
    }

    /// Parsed from the `"-<mechanism>"` value of the root's `_dd.p.dm` tag.
    fn sampling_mechanism(&self) -> Option<u32> {
        self.root?
            .attribute_str(DECISION_MAKER_KEY)?
            .strip_prefix('-')?
            .parse()
            .ok()
    }

    // These formats report dropped traces through a request header, not per trace.
    fn dropped_trace(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::SpanBytes;
    use libdd_trace_model::ChunkSpanView;
    use std::collections::HashMap;

    #[test]
    fn reads_chunk_fields_from_root() {
        let mut root = SpanBytes {
            trace_id: 7,
            span_id: 1,
            ..Default::default()
        };
        for (k, v) in [("_dd.origin", "synthetics"), ("_dd.p.dm", "-4")] {
            root.meta.insert(k.into(), v.into());
        }
        root.metrics.insert("_sampling_priority_v1".into(), 2.0);
        let mut child = SpanBytes {
            trace_id: 7,
            span_id: 2,
            parent_id: 1,
            ..Default::default()
        };
        child.meta.insert("_dd.origin".into(), "lambda".into());
        // Some clients put the root span last.
        let trace = [child, root];

        let chunk = RootSpanChunk::new(&trace);
        assert_eq!(chunk.trace_id(), 7);
        // The root's origin is the chunk's, whatever the other spans say.
        assert_eq!(chunk.origin(), "synthetics");
        assert_eq!(chunk.priority(), Some(2));
        assert_eq!(chunk.sampling_mechanism(), Some(4));
        assert!(!chunk.dropped_trace());
        assert_eq!(chunk.spans().count(), 2);
        let view = ChunkSpanView {
            span: &trace[0],
            chunk: &chunk,
        };
        assert_eq!(view.chunk.origin(), "synthetics");

        let empty = RootSpanChunk::<SpanBytes>::new(&[]);
        assert_eq!(
            (empty.trace_id(), empty.origin(), empty.priority()),
            (0, "", None)
        );
    }

    #[test]
    fn single_protobuf_span_is_its_own_root() {
        let span = pb::Span {
            trace_id: 7,
            span_id: 3,
            // The parent isn't in the chunk, so the span is still the root.
            parent_id: 2,
            meta: HashMap::from([("_dd.origin".to_owned(), "synthetics".to_owned())]),
            metrics: HashMap::from([("_sampling_priority_v1".to_owned(), 1.0)]),
            ..Default::default()
        };
        let chunk = RootSpanChunk::new(std::slice::from_ref(&span));
        assert_eq!(chunk.trace_id(), 7);
        assert_eq!(chunk.origin(), "synthetics");
        assert_eq!(chunk.priority(), Some(1));
    }
}
