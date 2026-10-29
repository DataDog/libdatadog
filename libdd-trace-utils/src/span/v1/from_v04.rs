// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! In-memory upgrade: v0.4 trace chunks → [`TraceChunk`]s.
//!
//! This is what lets v0.4/v0.5 input go through the v1-native pipeline: the msgpack decoders
//! (see [`crate::msgpack_decoder::v04::from_bytes_into_v1`]) and callers handing over
//! already-decoded v0.4 spans both end up here.
//!
//! The mapping mirrors the v0.4 → V1 wire encoder ([`crate::msgpack_encoder::v1`]'s
//! `*_from_v04` functions), so a v0.4 payload converted here and then encoded as V1 matches what
//! that encoder would have produced:
//!
//! | v0.4 field                                | V1 field                                   |
//! |-------------------------------------------|--------------------------------------------|
//! | `meta["env"]` / `["version"]` / `["component"]` | `env` / `version` / `component`      |
//! | `meta["span.kind"]`                       | `span_kind` (unknown values → `Internal`)  |
//! | `meta[k]`                                 | `AttributeValue::String`                   |
//! | `metrics[k]`                              | `AttributeValue::Float`                    |
//! | `meta_struct[k]`                          | `AttributeValue::Bytes`                    |
//! | `error: i32`                              | `error: bool` (non-zero → `true`)          |
//! | first span's `trace_id` + any span's `meta["_dd.p.tid"]` | chunk `trace_id` (128-bit big-endian) |
//! | root span's `metrics["_sampling_priority_v1"]` | chunk `priority`                      |
//! | root span's `meta["_dd.origin"]`          | chunk `origin`                             |
//! | root span's `meta["_dd.p.dm"]`            | chunk `sampling_mechanism` (absolute value)|
//!
//! `meta["_dd.p.tid"]` is consumed into the chunk `trace_id` and not kept as an attribute; it is
//! looked up on every span of the chunk, since per RFC #85 only the root carries it. The other
//! chunk-level tags stay on their spans as attributes too, like the wire encoder does.

use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink, ThinVec, TraceChunk};
use crate::span::v04;
use crate::span::vec_map::VecMap;
use crate::span::{SpanText, TraceData};
use std::borrow::Borrow;

/// Chunk-level values read from the v0.4 spans before they are consumed.
struct ChunkAttrs {
    trace_id: u128,
    priority: Option<i32>,
    origin: Option<String>,
    sampling_mechanism: Option<u32>,
}

fn sampling_priority<T: TraceData>(span: &v04::Span<T>) -> Option<i32> {
    // Sampling priorities are small integers carried as f64 in v0.4 `metrics`; `as` saturates
    // out-of-range values, matching the v0.4 → V1 wire encoder.
    span.metrics.get("_sampling_priority_v1").map(|v| *v as i32)
}

fn origin<T: TraceData>(span: &v04::Span<T>) -> Option<&str> {
    span.meta.get("_dd.origin").map(|v| v.borrow())
}

fn sampling_mechanism<T: TraceData>(span: &v04::Span<T>) -> Option<u32> {
    // `_dd.p.dm` is a signed integer stored as a string; `unsigned_abs` keeps the magnitude.
    span.meta
        .get("_dd.p.dm")
        .and_then(|v| v.borrow().parse::<i32>().ok())
        .map(|dm| dm.unsigned_abs())
}

fn extract_chunk_attrs<T: TraceData>(spans: &[v04::Span<T>]) -> ChunkAttrs {
    // trace_id is invariant per chunk: its low 64 bits are taken from the first span. The high 64
    // bits are resolved over the whole chunk, from either a span's native u128 `trace_id` or its
    // `_dd.p.tid` hex meta tag, which per RFC #85 is set on the chunk root only (and the root
    // isn't necessarily the first span).
    let high = spans
        .iter()
        .find_map(|s| {
            // Shifting a u128 right by 64 always fits in a u64.
            let native_high = (s.trace_id >> 64) as u64;
            if native_high != 0 {
                return Some(native_high);
            }
            s.meta
                .get("_dd.p.tid")
                .and_then(|v| u64::from_str_radix(v.borrow(), 16).ok())
        })
        .unwrap_or(0);
    let low = spans
        .first()
        .map_or(0, |s| s.trace_id & u128::from(u64::MAX));
    let trace_id = (u128::from(high) << 64) | low;

    let mut priority = None;
    let mut origin_tag = None;
    let mut mechanism = None;

    for span in spans {
        // Root span: either no parent in this chunk, or tagged _dd.top_level=1 (remote parent).
        let is_root =
            span.parent_id == 0 || span.metrics.get("_dd.top_level").copied().unwrap_or(0.0) == 1.0;

        if is_root {
            // The root span is authoritative, including for absent values.
            priority = sampling_priority(span);
            origin_tag = origin(span);
            mechanism = sampling_mechanism(span);
            break;
        }

        // No root found yet: fall back to the first non-root span carrying each value (partial
        // flush).
        priority = priority.or_else(|| sampling_priority(span));
        origin_tag = origin_tag.or_else(|| origin(span));
        mechanism = mechanism.or_else(|| sampling_mechanism(span));
    }

    ChunkAttrs {
        trace_id,
        priority,
        origin: origin_tag.map(str::to_owned),
        sampling_mechanism: mechanism,
    }
}

fn span_link_from_v04<T: TraceData>(link: v04::SpanLink<T>) -> SpanLink<T> {
    let trace_id = (u128::from(link.trace_id_high) << 64) | u128::from(link.trace_id);
    SpanLink {
        trace_id: trace_id.to_be_bytes(),
        span_id: link.span_id,
        attributes: link
            .attributes
            .into_iter()
            .map(|(k, v)| (k, AttributeValue::String(v)))
            .collect(),
        tracestate: link.tracestate,
        flags: link.flags,
    }
}

fn attribute_from_v04_array_value<T: TraceData>(
    value: v04::AttributeArrayValue<T>,
) -> AttributeValue<T> {
    match value {
        v04::AttributeArrayValue::String(s) => AttributeValue::String(s),
        v04::AttributeArrayValue::Boolean(b) => AttributeValue::Bool(b),
        v04::AttributeArrayValue::Integer(i) => AttributeValue::Int(i),
        v04::AttributeArrayValue::Double(d) => AttributeValue::Float(d),
    }
}

fn span_event_from_v04<T: TraceData>(event: v04::SpanEvent<T>) -> SpanEvent<T> {
    SpanEvent {
        time_unix_nano: event.time_unix_nano,
        name: event.name,
        attributes: event
            .attributes
            .into_iter()
            .map(|(k, v)| {
                let v = match v {
                    v04::AttributeAnyValue::SingleValue(v) => attribute_from_v04_array_value(v),
                    v04::AttributeAnyValue::Array(values) => AttributeValue::Array(
                        values
                            .into_iter()
                            .map(attribute_from_v04_array_value)
                            .collect(),
                    ),
                };
                (k, v)
            })
            .collect(),
    }
}

/// Converts a v0.4 span into a V1 span. Chunk-level fields (`trace_id`, sampling priority, ...)
/// are not part of a V1 span; use [`trace_chunk_from_v04`] to keep them.
pub fn span_from_v04<T: TraceData>(span: v04::Span<T>) -> Span<T> {
    let v04::Span {
        service,
        name,
        resource,
        r#type,
        trace_id: _,
        span_id,
        parent_id,
        start,
        duration,
        error,
        meta,
        metrics,
        meta_struct,
        span_links,
        span_events,
    } = span;

    let mut env = T::Text::default();
    let mut version = T::Text::default();
    let mut component = T::Text::default();
    let mut span_kind = SpanKind::Internal;
    let mut attributes = VecMap::with_capacity(meta.len() + metrics.len() + meta_struct.len());

    // Iterating in insertion order makes the last occurrence of a duplicated promoted key win,
    // like `VecMap::dedup` does for the remaining attributes.
    for (k, v) in meta {
        match k.borrow() {
            "env" => env = v,
            "version" => version = v,
            "component" => component = v,
            "span.kind" => span_kind = SpanKind::from_meta(v.borrow()),
            // Consumed into the chunk-level trace_id.
            "_dd.p.tid" => {}
            _ => attributes.insert(k, AttributeValue::String(v)),
        }
    }
    for (k, v) in metrics {
        attributes.insert(k, AttributeValue::Float(v));
    }
    for (k, v) in meta_struct {
        attributes.insert(k, AttributeValue::Bytes(v));
    }

    Span {
        service,
        name,
        resource,
        r#type,
        span_id,
        parent_id,
        start,
        duration,
        error: error != 0,
        span_kind,
        env,
        version,
        component,
        attributes,
        span_links: span_links
            .into_iter()
            .map(span_link_from_v04)
            .collect::<ThinVec<_>>(),
        span_events: span_events
            .into_iter()
            .map(span_event_from_v04)
            .collect::<ThinVec<_>>(),
    }
}

/// Converts a v0.4 trace chunk (a list of spans sharing a trace ID) into a [`TraceChunk`].
///
/// See the [module docs](self) for the mapping.
pub fn trace_chunk_from_v04<T: TraceData>(spans: Vec<v04::Span<T>>) -> TraceChunk<T> {
    let attrs = extract_chunk_attrs(&spans);
    TraceChunk {
        trace_id: attrs.trace_id.to_be_bytes(),
        priority: attrs.priority,
        origin: attrs.origin.map(T::Text::from_owned).unwrap_or_default(),
        sampling_mechanism: attrs.sampling_mechanism,
        dropped_trace: false,
        attributes: VecMap::new(),
        spans: spans.into_iter().map(span_from_v04).collect(),
    }
}

/// Converts a list of v0.4 trace chunks into [`TraceChunk`]s. See [`trace_chunk_from_v04`].
pub fn trace_chunks_from_v04<T: TraceData>(traces: Vec<Vec<v04::Span<T>>>) -> Vec<TraceChunk<T>> {
    traces.into_iter().map(trace_chunk_from_v04).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v1::TraceChunkBytes;
    use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, SpanBytes as V04SpanBytes};
    use libdd_tinybytes::{Bytes, BytesString};
    use std::collections::HashMap;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap()
    }

    fn attr<'a>(
        chunk: &'a TraceChunkBytes,
        span: usize,
        key: &str,
    ) -> Option<&'a AttributeValue<crate::span::BytesData>> {
        chunk.spans[span].attributes.get(key)
    }

    #[test]
    fn promotes_dedicated_fields_and_types_attributes() {
        let mut span = V04SpanBytes {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            r#type: bs("web"),
            trace_id: 0x1234,
            span_id: 2,
            parent_id: 0,
            start: 10,
            duration: 20,
            error: 3,
            ..Default::default()
        };
        span.meta.insert(bs("env"), bs("prod"));
        span.meta.insert(bs("version"), bs("1.0"));
        span.meta.insert(bs("component"), bs("http"));
        span.meta.insert(bs("span.kind"), bs("client"));
        span.meta.insert(bs("_dd.p.tid"), bs("00000000000000ab"));
        span.meta.insert(bs("_dd.origin"), bs("synthetics"));
        span.meta.insert(bs("_dd.p.dm"), bs("-4"));
        span.meta.insert(bs("custom"), bs("value"));
        span.metrics.insert(bs("_sampling_priority_v1"), 2.0);
        span.metrics.insert(bs("m"), 1.5);
        span.meta_struct
            .insert(bs("ms"), Bytes::from_static(b"\x01\x02"));

        let chunk = trace_chunk_from_v04(vec![span]);

        let mut expected_trace_id = [0u8; 16];
        expected_trace_id[7] = 0xab;
        expected_trace_id[14] = 0x12;
        expected_trace_id[15] = 0x34;
        assert_eq!(chunk.trace_id, expected_trace_id);
        assert_eq!(chunk.priority, Some(2));
        assert_eq!(chunk.origin, bs("synthetics"));
        assert_eq!(chunk.sampling_mechanism, Some(4));
        assert!(!chunk.dropped_trace);

        let s = &chunk.spans[0];
        assert_eq!(s.service, bs("svc"));
        assert_eq!(s.r#type, bs("web"));
        assert_eq!(s.span_id, 2);
        assert_eq!(s.start, 10);
        assert_eq!(s.duration, 20);
        assert!(s.error);
        assert_eq!(s.env, bs("prod"));
        assert_eq!(s.version, bs("1.0"));
        assert_eq!(s.component, bs("http"));
        assert_eq!(s.span_kind, SpanKind::Client);

        for promoted in ["env", "version", "component", "span.kind", "_dd.p.tid"] {
            assert!(attr(&chunk, 0, promoted).is_none(), "{promoted} kept");
        }
        assert_eq!(
            attr(&chunk, 0, "custom"),
            Some(&AttributeValue::String(bs("value")))
        );
        assert_eq!(attr(&chunk, 0, "m"), Some(&AttributeValue::Float(1.5)));
        assert_eq!(
            attr(&chunk, 0, "ms"),
            Some(&AttributeValue::Bytes(Bytes::from_static(b"\x01\x02")))
        );
        // Chunk-level tags stay on the span too, like the v0.4 → V1 wire encoder.
        assert_eq!(
            attr(&chunk, 0, "_dd.origin"),
            Some(&AttributeValue::String(bs("synthetics")))
        );
        assert_eq!(
            attr(&chunk, 0, "_sampling_priority_v1"),
            Some(&AttributeValue::Float(2.0))
        );
    }

    #[test]
    fn root_span_is_authoritative_for_chunk_attrs() {
        let mut child = V04SpanBytes {
            span_id: 2,
            parent_id: 1,
            ..Default::default()
        };
        child.metrics.insert(bs("_sampling_priority_v1"), 1.0);
        child.meta.insert(bs("_dd.origin"), bs("child"));
        let mut root = V04SpanBytes {
            span_id: 1,
            ..Default::default()
        };
        root.metrics.insert(bs("_sampling_priority_v1"), -1.0);

        let chunk = trace_chunk_from_v04(vec![child, root]);
        assert_eq!(chunk.priority, Some(-1));
        // Absent on the root: the child's value is not used.
        assert_eq!(chunk.origin, bs(""));
    }

    #[test]
    fn partial_flush_falls_back_to_non_root_spans() {
        let mut first = V04SpanBytes {
            span_id: 2,
            parent_id: 1,
            ..Default::default()
        };
        first.meta.insert(bs("_dd.origin"), bs("lambda"));
        let mut second = V04SpanBytes {
            span_id: 3,
            parent_id: 1,
            ..Default::default()
        };
        second.metrics.insert(bs("_sampling_priority_v1"), 1.0);

        let chunk = trace_chunk_from_v04(vec![first, second]);
        assert_eq!(chunk.priority, Some(1));
        assert_eq!(chunk.origin, bs("lambda"));
        assert_eq!(chunk.sampling_mechanism, None);
    }

    #[test]
    fn high_trace_id_is_read_from_a_later_root_span() {
        // RFC #85: `_dd.p.tid` is only set on the chunk root, which may not come first.
        let child = V04SpanBytes {
            trace_id: 0x1234,
            span_id: 2,
            parent_id: 1,
            ..Default::default()
        };
        let mut root = V04SpanBytes {
            trace_id: 0x1234,
            span_id: 1,
            ..Default::default()
        };
        root.meta.insert(bs("_dd.p.tid"), bs("00000000000000ab"));

        let chunk = trace_chunk_from_v04(vec![child, root]);
        let mut expected_trace_id = [0u8; 16];
        expected_trace_id[7] = 0xab;
        expected_trace_id[14] = 0x12;
        expected_trace_id[15] = 0x34;
        assert_eq!(chunk.trace_id, expected_trace_id);
        assert!(attr(&chunk, 1, "_dd.p.tid").is_none());
    }

    #[test]
    fn high_trace_id_prefers_native_u128_trace_id() {
        let span = V04SpanBytes {
            trace_id: (0xcd_u128 << 64) | 0x1234,
            span_id: 1,
            ..Default::default()
        };
        let chunk = trace_chunk_from_v04(vec![span]);
        assert_eq!(chunk.trace_id[7], 0xcd);
        assert_eq!(chunk.trace_id[15], 0x34);
    }

    #[test]
    fn converts_span_links_and_events() {
        let span = V04SpanBytes {
            span_links: vec![v04::SpanLink {
                trace_id: 0x2,
                trace_id_high: 0x1,
                span_id: 3,
                attributes: HashMap::from([(bs("k"), bs("v"))]),
                tracestate: bs("ts"),
                flags: 1,
            }],
            span_events: vec![v04::SpanEvent {
                time_unix_nano: 42,
                name: bs("ev"),
                attributes: HashMap::from([
                    (
                        bs("s"),
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(7)),
                    ),
                    (
                        bs("a"),
                        AttributeAnyValue::Array(vec![
                            AttributeArrayValue::Boolean(true),
                            AttributeArrayValue::Double(0.5),
                            AttributeArrayValue::String(bs("x")),
                        ]),
                    ),
                ]),
            }],
            ..Default::default()
        };

        let chunk = trace_chunk_from_v04(vec![span]);
        let s = &chunk.spans[0];

        let link = &s.span_links[0];
        let mut expected_trace_id = [0u8; 16];
        expected_trace_id[7] = 1;
        expected_trace_id[15] = 2;
        assert_eq!(link.trace_id, expected_trace_id);
        assert_eq!(link.span_id, 3);
        assert_eq!(link.tracestate, bs("ts"));
        assert_eq!(link.flags, 1);
        assert_eq!(
            link.attributes.get("k"),
            Some(&AttributeValue::String(bs("v")))
        );

        let event = &s.span_events[0];
        assert_eq!(event.time_unix_nano, 42);
        assert_eq!(event.name, bs("ev"));
        assert_eq!(event.attributes.get("s"), Some(&AttributeValue::Int(7)));
        assert_eq!(
            event.attributes.get("a"),
            Some(&AttributeValue::Array(vec![
                AttributeValue::Bool(true),
                AttributeValue::Float(0.5),
                AttributeValue::String(bs("x")),
            ]))
        );
    }

    #[test]
    fn unknown_span_kind_maps_to_internal() {
        let mut span = V04SpanBytes::default();
        span.meta.insert(bs("span.kind"), bs("weird"));
        let s = span_from_v04(span);
        assert_eq!(s.span_kind, SpanKind::Internal);
        assert!(s.attributes.get("span.kind").is_none());
    }

    #[test]
    fn empty_chunk() {
        let chunk = trace_chunk_from_v04::<crate::span::BytesData>(vec![]);
        assert_eq!(chunk.trace_id, [0; 16]);
        assert!(chunk.spans.is_empty());
        assert_eq!(chunk.priority, None);
    }
}
