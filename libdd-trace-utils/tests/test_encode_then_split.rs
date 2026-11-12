// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_tinybytes::BytesString;
use libdd_trace_utils::{
    msgpack_decoder,
    msgpack_encoder::{
        split::{self, Format, Outcome, Part},
        v1, v04,
    },
    span::v1::{
        AttributeValue, SpanBytes, SpanEventBytes, SpanLinkBytes, TraceChunkBytes,
        TracerPayloadBytes,
    },
};

fn text(value: impl Into<String>) -> BytesString {
    BytesString::from(value.into())
}

fn payload() -> TracerPayloadBytes {
    let mut payload = TracerPayloadBytes {
        env: text("production"),
        app_version: text("1.2"),
        runtime_id: text("runtime"),
        hostname: text("host"),
        container_id: text("container"),
        language_name: text("ruby"),
        chunks: (0..3u64)
            .map(|i| {
                let mut chunk = TraceChunkBytes {
                    trace_id: (u128::from(i + 1) << 64 | 17).to_be_bytes(),
                    priority: Some(2),
                    sampling_mechanism: Some(8),
                    origin: text("synthetics"),
                    spans: (0..5u64)
                        .map(|j| {
                            let mut span = SpanBytes {
                                service: text("web"),
                                name: text("request"),
                                resource: text(format!("{i}/{j}/{}", "x".repeat(80))),
                                span_id: i * 100 + j + 1,
                                parent_id: if j == 0 { 0 } else { i * 100 + 1 },
                                start: 1000,
                                duration: 50,
                                ..Default::default()
                            };
                            span.attributes.insert(
                                text("_dd.top_level"),
                                AttributeValue::Int(i64::from(j == 0)),
                            );
                            span.span_links.push(SpanLinkBytes {
                                trace_id: 19u128.to_be_bytes(),
                                span_id: 99,
                                tracestate: text("dd=s:1"),
                                ..Default::default()
                            });
                            span.span_events.push(SpanEventBytes {
                                name: text("event"),
                                time_unix_nano: 12,
                                ..Default::default()
                            });
                            span
                        })
                        .collect(),
                    ..Default::default()
                };
                chunk.attributes.insert(
                    text("chunk.key"),
                    AttributeValue::String(text("chunk-value")),
                );
                chunk
            })
            .collect(),
        ..Default::default()
    };
    payload
        .attributes
        .insert(text("payload.key"), AttributeValue::Int(9));
    payload
}

fn reference(payload: &TracerPayloadBytes, format: Format) -> Vec<u8> {
    match format {
        Format::V04 => v04::to_vec_from_v1(payload),
        Format::V1 => v1::to_vec_from_v1(payload),
    }
}

fn selected(part: &Part) -> TracerPayloadBytes {
    let mut source = payload();
    source.chunks = source
        .chunks
        .into_iter()
        .enumerate()
        .filter_map(|(i, mut chunk)| {
            if !part.chunks().contains(&i) {
                return None;
            }
            if let Some(range) = part.spans() {
                chunk.spans = chunk.spans.drain(range).collect();
            }
            Some(chunk)
        })
        .collect();
    source
}

#[test]
fn fitting_payload_is_byte_identical_including_the_exact_limit() {
    let source = payload();
    for format in [Format::V04, Format::V1] {
        let expected = reference(&source, format);
        for limit in [expected.len(), expected.len() + 1] {
            let mut results = split::encode(&source, format, limit);
            let Some(Outcome::Encoded { part, bytes }) = results.next() else {
                panic!("expected payload")
            };
            assert_eq!(bytes, expected);
            assert_eq!(part.chunks(), 0..3);
            assert_eq!(part.spans(), None);
            assert!(results.next().is_none());
            assert!(results.next().is_none());
        }
    }
}

#[test]
fn whole_chunks_are_split_before_spans() {
    let source = payload();
    for format in [Format::V04, Format::V1] {
        let limit = (0..3)
            .map(|i| {
                let mut single = payload();
                single.chunks = vec![single.chunks.remove(i)];
                reference(&single, format).len()
            })
            .max()
            .unwrap();
        assert!(reference(&source, format).len() > limit);
        let mut seen = Vec::new();
        for result in split::encode(&source, format, limit) {
            let Outcome::Encoded { part, bytes } = result else {
                panic!("unexpected drop")
            };
            assert!(bytes.len() <= limit);
            assert!(part.spans().is_none());
            assert_eq!(bytes, reference(&selected(&part), format));
            seen.extend(part.chunks());
        }
        assert_eq!(seen, vec![0, 1, 2]);
    }
}

#[test]
fn all_limits_partition_every_original_span_once_in_order() {
    let source = payload();
    for format in [Format::V04, Format::V1] {
        let size = reference(&source, format).len();
        for limit in [0, 1, 250, 400, 700, size / 2, size - 1] {
            let mut seen = Vec::new();
            for outcome in split::encode(&source, format, limit) {
                let part = match outcome {
                    Outcome::Encoded { part, bytes } => {
                        assert!(bytes.len() <= limit);
                        assert_eq!(bytes, reference(&selected(&part), format));
                        part
                    }
                    Outcome::Oversized { part, encoded_size } => {
                        assert_eq!(part.chunks().len(), 1);
                        assert_eq!(part.spans().unwrap().len(), 1);
                        assert_eq!(encoded_size, reference(&selected(&part), format).len());
                        assert!(encoded_size > limit);
                        part
                    }
                };
                for chunk in part.chunks() {
                    for span in part.spans().unwrap_or(0..source.chunks[chunk].spans.len()) {
                        seen.push((chunk, span));
                    }
                }
            }
            assert_eq!(
                seen,
                (0..3)
                    .flat_map(|c| (0..5).map(move |s| (c, s)))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn rootless_pieces_keep_the_original_trace_and_sampling_metadata() {
    let mut source = payload();
    source.chunks.truncate(1);
    source.chunks[0].dropped_trace = true;
    source.chunks[0].priority = Some(-1);
    let mut rootless = false;
    for result in split::encode(&source, Format::V1, 450) {
        let Outcome::Encoded { bytes, .. } = result else {
            panic!("unexpected drop")
        };
        let (decoded, _) = msgpack_decoder::v1::from_slice(&bytes).unwrap();
        assert_eq!(decoded.env, "production");
        assert_eq!(decoded.app_version, "1.2");
        assert_eq!(decoded.runtime_id, "runtime");
        assert_eq!(
            decoded.attributes.get("payload.key"),
            Some(&AttributeValue::Int(9))
        );
        assert_eq!(decoded.chunks.len(), 1);
        let chunk = &decoded.chunks[0];
        assert_eq!(chunk.trace_id, source.chunks[0].trace_id);
        assert_eq!(chunk.priority, Some(-1));
        assert_eq!(chunk.sampling_mechanism, Some(8));
        assert_eq!(chunk.origin, "synthetics");
        assert!(chunk.dropped_trace);
        assert_eq!(
            chunk.attributes.get("chunk.key").and_then(|a| a.as_str()),
            Some("chunk-value")
        );
        for span in &chunk.spans {
            assert_eq!(span.parent_id, if span.span_id == 1 { 0 } else { 1 });
            assert_eq!(
                span.attributes.get("_dd.top_level"),
                Some(&AttributeValue::Int(i64::from(span.span_id == 1)))
            );
            assert_eq!(span.span_events[0].name, "event");
            assert_eq!(span.span_links[0].span_id, 99);
        }
        rootless |= chunk.spans.iter().all(|span| span.span_id != 1);
    }
    assert!(rootless);
}

#[test]
fn v04_downgrade_keeps_chunk_context_on_rootless_pieces() {
    let mut source = payload();
    source.chunks.truncate(1);
    let mut rootless = false;
    for result in split::encode(&source, Format::V04, 750) {
        let Outcome::Encoded { bytes, .. } = result else {
            panic!("unexpected drop")
        };
        let (chunks, _) = msgpack_decoder::v04::from_slice(&bytes).unwrap();
        for spans in &chunks {
            rootless |= spans.iter().all(|span| span.span_id != 1);
            for span in spans {
                assert_eq!(span.trace_id, 17);
                assert_eq!(span.meta.get("_dd.p.tid").unwrap(), "0000000000000001");
                assert_eq!(span.meta.get("_dd.origin").unwrap(), "synthetics");
                assert_eq!(span.metrics.get("_sampling_priority_v1"), Some(&2.0));
                assert_eq!(span.meta.get("chunk.key").unwrap(), "chunk-value");
                assert_eq!(span.metrics.get("payload.key"), Some(&9.0));
            }
        }
    }
    assert!(rootless);
}

#[test]
fn a_single_oversized_span_does_not_discard_its_neighbours() {
    for format in [Format::V04, Format::V1] {
        let mut source = payload();
        source.chunks.truncate(1);
        source.chunks[0].spans[2].resource = text("x".repeat(4096));
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for result in split::encode(&source, format, 1024) {
            match result {
                Outcome::Encoded { part, bytes } => {
                    assert!(bytes.len() <= 1024);
                    kept.extend(part.spans().unwrap());
                }
                Outcome::Oversized { part, .. } => dropped.extend(part.spans().unwrap()),
            }
        }
        assert_eq!(kept, vec![0, 1, 3, 4]);
        assert_eq!(dropped, vec![2]);
    }
}

#[test]
fn empty_chunks_and_oversized_envelopes_terminate() {
    for format in [Format::V04, Format::V1] {
        let source = TracerPayloadBytes::default();
        let expected = reference(&source, format);
        let mut outcomes = split::encode(&source, format, expected.len() - 1);
        assert!(
            matches!(outcomes.next(), Some(Outcome::Oversized { part, .. }) if part.chunks().is_empty())
        );
        assert!(outcomes.next().is_none());
        let mut source = payload();
        source.chunks[0].spans.clear();
        source.chunks.truncate(1);
        assert_eq!(split::encode(&source, format, 0).count(), 1);
    }
}

#[test]
fn indivisible_chunks_including_empty_neighbours_are_reported() {
    let mut source = payload();
    source.chunks[0].spans.clear();
    source.chunks[1].spans.truncate(1);
    source.chunks[2].spans.clear();
    for format in [Format::V04, Format::V1] {
        let mut seen = Vec::new();
        for outcome in split::encode(&source, format, 0) {
            let Outcome::Oversized { part, encoded_size } = outcome else {
                panic!("expected oversized envelope");
            };
            assert_eq!(part.chunks().len(), 1);
            assert_eq!(part.spans(), None);
            assert!(encoded_size > 0);
            seen.extend(part.chunks());
        }
        assert_eq!(seen, vec![0, 1, 2]);
    }
}
