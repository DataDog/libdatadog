// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_tinybytes::BytesString;
use libdd_trace_utils::{
    msgpack_decoder,
    msgpack_encoder::{
        split::{self, Format, Outcome},
        v1, v04,
    },
    span::v1::{
        AttributeValue, SpanBytes, SpanEventBytes, SpanLinkBytes, TraceChunkBytes,
        TracerPayloadBytes,
    },
};
use std::ops::Range;

fn text(value: impl Into<String>) -> BytesString {
    BytesString::from(value.into())
}

fn payload(chunks: Range<usize>) -> TracerPayloadBytes {
    let mut payload = TracerPayloadBytes {
        env: text("production"),
        app_version: text("1.2"),
        runtime_id: text("runtime"),
        hostname: text("host"),
        container_id: text("container"),
        language_name: text("ruby"),
        chunks: chunks
            .map(|i| {
                let i = u64::try_from(i).unwrap();
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

#[test]
fn fitting_payload_is_byte_identical_including_the_exact_limit() {
    let source = payload(0..3);
    for format in [Format::V04, Format::V1] {
        let expected = reference(&source, format);
        for limit in [expected.len(), expected.len() + 1] {
            let mut results = split::encode(&source, format, limit);
            let Some(Outcome::Encoded { chunks, bytes }) = results.next() else {
                panic!("expected payload")
            };
            assert_eq!(bytes, expected);
            assert_eq!(chunks, 0..3);
            assert!(results.next().is_none());
            assert!(results.next().is_none());
        }
    }
}

#[test]
fn fitting_chunk_ranges_stay_together() {
    let source = payload(0..3);
    for format in [Format::V04, Format::V1] {
        let limit = reference(&payload(1..3), format).len();
        assert!(reference(&source, format).len() > limit);
        let mut results = split::encode(&source, format, limit);
        for expected in [0..1, 1..3] {
            let Some(Outcome::Encoded { chunks, bytes }) = results.next() else {
                panic!("unexpected drop")
            };
            assert_eq!(chunks, expected);
            assert!(bytes.len() <= limit);
            assert_eq!(bytes, reference(&payload(expected), format));
        }
        assert!(results.next().is_none());
    }
}

#[test]
fn outcomes_cover_every_original_chunk_once_in_order() {
    let source = payload(0..3);
    for format in [Format::V04, Format::V1] {
        let size = reference(&source, format).len();
        let single = reference(&payload(0..1), format).len();
        for limit in [0, single - 1, single, size / 2, size - 1] {
            let mut seen = Vec::new();
            for outcome in split::encode(&source, format, limit) {
                let chunks = match outcome {
                    Outcome::Encoded { chunks, bytes } => {
                        assert!(bytes.len() <= limit);
                        assert_eq!(bytes, reference(&payload(chunks.clone()), format));
                        chunks
                    }
                    Outcome::Oversized {
                        chunks,
                        encoded_size,
                    } => {
                        assert_eq!(chunks.len(), 1);
                        assert_eq!(
                            encoded_size,
                            reference(&payload(chunks.clone()), format).len()
                        );
                        assert!(encoded_size > limit);
                        chunks
                    }
                };
                seen.extend(chunks);
            }
            assert_eq!(seen, [0, 1, 2]);
        }
    }
}

fn roots_last_payload() -> TracerPayloadBytes {
    let mut source = payload(0..2);
    for chunk in &mut source.chunks {
        chunk.dropped_trace = true;
        chunk.spans[0]
            .attributes
            .insert(text("sdk.root"), AttributeValue::String(text("root-only")));
        chunk.spans.rotate_left(1);
    }
    for (key, value) in [
        ("_dd.git.commit.sha", "commit"),
        ("_dd.tags.process", "process:tag"),
        ("_dd.sdk.otlp_export", "false"),
    ] {
        source
            .attributes
            .insert(text(key), AttributeValue::String(text(value)));
    }
    source
}

#[test]
fn v1_keeps_sdk_root_metadata_with_its_chunk() {
    let source = roots_last_payload();
    let limit = v1::to_vec_from_v1(&source).len() - 1;
    let mut results = split::encode(&source, Format::V1, limit);
    for (i, original) in source.chunks.iter().enumerate() {
        let Some(Outcome::Encoded { chunks, bytes }) = results.next() else {
            panic!("unexpected drop")
        };
        assert_eq!(chunks, i..i + 1);
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
        assert_eq!(chunk.trace_id, original.trace_id);
        assert_eq!(chunk.priority, Some(2));
        assert_eq!(chunk.sampling_mechanism, Some(8));
        assert_eq!(chunk.origin, "synthetics");
        assert!(!chunk.dropped_trace);
        assert_eq!(
            chunk.attributes.get("chunk.key").and_then(|a| a.as_str()),
            Some("chunk-value")
        );
        assert_eq!(chunk.spans.len(), original.spans.len());
        for (span, input) in chunk.spans.iter().zip(&original.spans) {
            assert_eq!(
                (span.span_id, span.parent_id),
                (input.span_id, input.parent_id)
            );
            let root = input.parent_id == 0;
            assert_eq!(
                span.attributes.get("sdk.root").and_then(|a| a.as_str()),
                root.then_some("root-only")
            );
            assert_eq!(
                span.attributes.get("_dd.top_level"),
                Some(&AttributeValue::Int(i64::from(root)))
            );
            assert_eq!(span.span_events[0].name, "event");
            assert_eq!(span.span_links[0].span_id, 99);
        }
    }
    assert!(results.next().is_none());
}

#[test]
fn v04_downgrade_keeps_metadata_on_the_original_root_and_first_span() {
    let source = roots_last_payload();
    let limit = v04::to_vec_from_v1(&source).len() - 1;
    let mut results = split::encode(&source, Format::V04, limit);
    for (i, original) in source.chunks.iter().enumerate() {
        let Some(Outcome::Encoded { chunks, bytes }) = results.next() else {
            panic!("unexpected drop")
        };
        assert_eq!(chunks, i..i + 1);
        let (chunks, _) = msgpack_decoder::v04::from_slice(&bytes).unwrap();
        assert_eq!(chunks.len(), 1);
        let spans = &chunks[0];
        assert_eq!(spans.len(), original.spans.len());
        let high_trace_id = format!("{:016x}", i + 1);
        for (span, input) in spans.iter().zip(&original.spans) {
            assert_eq!(span.span_id, input.span_id);
            assert_eq!(span.trace_id, 17);
            let root = input.parent_id == 0;
            for (key, value) in [
                ("sdk.root", "root-only"),
                ("_dd.p.tid", high_trace_id.as_str()),
                ("_dd.origin", "synthetics"),
                ("_dd.p.dm", "-8"),
                ("_dd.hostname", "host"),
                ("_dd.git.commit.sha", "commit"),
            ] {
                assert_eq!(
                    span.meta.get(key).map(|value| value.as_ref()),
                    root.then_some(value)
                );
            }
            assert_eq!(
                span.metrics.get("_sampling_priority_v1"),
                root.then_some(&2.0)
            );
            let first = input.span_id == original.spans[0].span_id;
            for (key, value) in [
                ("_dd.tags.process", "process:tag"),
                ("_dd.sdk.otlp_export", "false"),
            ] {
                assert_eq!(
                    span.meta.get(key).map(|value| value.as_ref()),
                    first.then_some(value)
                );
            }
            assert_eq!(span.meta.get("chunk.key").unwrap(), "chunk-value");
            assert_eq!(span.metrics.get("payload.key"), Some(&9.0));
        }
    }
    assert!(results.next().is_none());
}

#[test]
fn oversized_chunk_is_reported_even_if_its_spans_would_fit_separately() {
    let singleton = |spans| {
        let mut source = payload(0..1);
        let chunk = &mut source.chunks[0];
        chunk.spans = chunk.spans.drain(spans).collect();
        source
    };
    let source = singleton(0..2);
    for format in [Format::V1, Format::V04] {
        let size = reference(&source, format).len();
        let limit = reference(&singleton(0..1), format)
            .len()
            .max(reference(&singleton(1..2), format).len());
        assert!(size > limit);
        let mut results = split::encode(&source, format, limit);
        let Some(Outcome::Oversized {
            chunks,
            encoded_size,
        }) = results.next()
        else {
            panic!("expected the complete {size}-byte chunk at limit {limit} ({format:?})");
        };
        assert_eq!(chunks, 0..1);
        assert_eq!(encoded_size, size);
        assert!(results.next().is_none());
    }
}

#[test]
fn an_oversized_chunk_does_not_discard_its_neighbours() {
    let mut source = payload(0..3);
    source.chunks[0].spans.truncate(1);
    source.chunks[2].spans.truncate(1);
    for format in [Format::V04, Format::V1] {
        let size = reference(&payload(1..2), format).len();
        let limit = size - 1;
        let mut results = split::encode(&source, format, limit);
        for i in 0..3 {
            let chunks = match results.next().unwrap() {
                Outcome::Encoded { chunks, bytes } => {
                    assert_ne!(i, 1);
                    assert!(bytes.len() <= limit);
                    chunks
                }
                Outcome::Oversized {
                    chunks,
                    encoded_size,
                } => {
                    assert_eq!(i, 1);
                    assert_eq!(encoded_size, size);
                    chunks
                }
            };
            assert_eq!(chunks, i..i + 1);
        }
        assert!(results.next().is_none());
    }
}

#[test]
fn oversized_empty_envelope_is_reported_once() {
    for format in [Format::V04, Format::V1] {
        let source = TracerPayloadBytes::default();
        let expected = reference(&source, format);
        let mut outcomes = split::encode(&source, format, expected.len() - 1);
        let Some(Outcome::Oversized {
            chunks,
            encoded_size,
        }) = outcomes.next()
        else {
            panic!("expected oversized empty payload");
        };
        assert_eq!(chunks, 0..0);
        assert_eq!(encoded_size, expected.len());
        assert!(outcomes.next().is_none());
    }
}

#[test]
fn indivisible_chunks_including_empty_neighbours_are_reported() {
    let mut source = payload(0..3);
    source.chunks[0].spans.clear();
    source.chunks[1].spans.truncate(1);
    source.chunks[2].spans.clear();
    for format in [Format::V04, Format::V1] {
        let mut results = split::encode(&source, format, 0);
        for (i, original) in source.chunks.iter().enumerate() {
            let Some(Outcome::Oversized {
                chunks,
                encoded_size,
            }) = results.next()
            else {
                panic!("expected oversized chunk");
            };
            assert_eq!(chunks, i..i + 1);
            let mut single = payload(i..i + 1);
            single.chunks[0].spans.truncate(original.spans.len());
            assert_eq!(encoded_size, reference(&single, format).len());
        }
        assert!(results.next().is_none());
    }
}
