// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace-utils functionalities implementation for V1 spans.

use crate::span::TraceData;
use crate::span::trace_utils::DroppedP0Stats;
use crate::span::v1::span_pool::PooledTraceChunks;
use crate::span::v1::{AttributeValue, Span};
use libdd_trace_model::{
    Attributes as _, AttributesIter, Span as _, SpanMut, TraceChunkMut, TraceText,
};
use std::collections::HashMap;

/// Span attribute the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
const SAMPLING_SINGLE_SPAN_MECHANISM: &str = "_dd.span_sampling.mechanism";
const SAMPLING_ANALYTICS_RATE_KEY: &str = "_dd1.sr.eausr";

fn set_top_level_span<S: SpanMut>(span: &mut S) {
    span.set_attribute(
        S::Text::from_static(TOP_LEVEL_KEY),
        AttributeValue::Float(1.0),
    );
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span<S: SpanMut>(trace: &mut [S]) {
    let span_id_idx: HashMap<u64, usize> = trace
        .iter()
        .enumerate()
        .map(|(i, span)| (span.span_id(), i))
        .collect();
    let top_level: Vec<usize> = trace
        .iter()
        .enumerate()
        .filter(|(_, span)| {
            span.parent_id() == 0
                || span_id_idx
                    .get(&span.parent_id())
                    .is_none_or(|&parent_idx| trace[parent_idx].service() != span.service())
        })
        .map(|(i, _)| i)
        .collect();
    for i in top_level {
        set_top_level_span(&mut trace[i]);
    }
}

/// Returns true if the span has a top level key set
pub fn has_top_level<T: TraceData>(span: &Span<T>) -> bool {
    span.has_top_level()
}

/// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured<T: TraceData>(span: &Span<T>) -> bool {
    span.is_measured()
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has an attribute _dd.partial_version which is a positive
/// integer. This value usually increases each time a new version of the same span is sent by
/// the tracer
pub fn is_partial_snapshot<T: TraceData>(span: &Span<T>) -> bool {
    span.is_partial_snapshot()
}

/// Remove spans and chunks, only keeping the ones that may be sampled by the agent.
///
/// Unlike v0.4 (where sampling priority is a per-span metric), v1's `priority` is already a
/// direct field on [`TraceChunk`](crate::span::v1::TraceChunk), so it is read once per chunk
/// instead of being searched for across the chunk's spans.
///
/// # Returns
///
/// The number of P0 traces and spans that were dropped.
pub fn drop_chunks<T: TraceData>(traces: &mut PooledTraceChunks<'_, T>) -> DroppedP0Stats {
    let mut stats = DroppedP0Stats {
        dropped_p0_traces: 0,
        dropped_p0_spans: 0,
    };
    traces.retain_mut(|chunk| drop_p0_spans(chunk, &mut stats));
    stats
}

/// Drops the spans of `chunk` the agent won't sample, returning false if the whole chunk should
/// be dropped. Dropped traces and spans are counted in `stats`.
fn drop_p0_spans<C>(chunk: &mut C, stats: &mut DroppedP0Stats) -> bool
where
    C: TraceChunkMut,
    C::Span: AttributesIter,
{
    // ErrorSampler
    if chunk.spans().any(|s| s.is_error()) {
        // We send chunks containing an error
        return true;
    }

    // PrioritySampler and NoPrioritySampler
    //
    // `dropped_trace` signals the trace was rejected by samplers even when `priority` is
    // absent or positive; treat it as an implicit negative priority (mirrors the
    // v1-to-v0.4 encoder's handling of this flag), unless the chunk already carries a
    // negative priority of its own.
    let effective_priority = if chunk.dropped_trace() {
        chunk.priority().filter(|&p| p < 0).or(Some(-1))
    } else {
        chunk.priority()
    };
    if effective_priority.is_none_or(|p| p > 0) {
        // We send chunks with positive priority or no priority
        return true;
    }

    // SingleSpanSampler and AnalyzedSpansSampler
    // Keep only the spans sampled by single-span sampling or analyzed spans, even if the
    // chunk is dropped.
    let mut kept = 0;
    chunk.retain_spans(|span| {
        let keep = span.attribute_f64(SAMPLING_SINGLE_SPAN_MECHANISM) == Some(8.0)
            || span.has_attribute(SAMPLING_ANALYTICS_RATE_KEY);
        if keep {
            kept += 1;
        } else {
            stats.dropped_p0_spans += 1;
        }
        keep
    });
    if kept == 0 {
        // If no spans were sampled we can drop the whole chunk
        stats.dropped_p0_traces += 1;
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::trace_utils::get_root_span_index;
    use crate::span::v1::{SpanBytes, TraceChunkBytes};

    const MEASURED_KEY: &str = "_dd.measured";
    const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";

    fn create_test_span(is_top_level: bool) -> SpanBytes {
        let mut span = SpanBytes {
            service: "test-service".into(),
            name: "test_name".into(),
            resource: "test-resource".into(),
            ..Default::default()
        };
        if is_top_level {
            span.attributes
                .insert("_top_level".into(), AttributeValue::Float(1.0));
        }
        span
    }

    fn create_test_span_with_ids(span_id: u64, parent_id: u64) -> SpanBytes {
        SpanBytes {
            service: "test-service".into(),
            name: "test_name".into(),
            resource: "test-resource".into(),
            span_id,
            parent_id,
            ..Default::default()
        }
    }

    #[test]
    fn test_has_top_level() {
        let top_level_span = create_test_span(true);
        let not_top_level_span = create_test_span(false);
        assert!(has_top_level(&top_level_span));
        assert!(!has_top_level(&not_top_level_span));
    }

    #[test]
    fn test_is_measured() {
        let mut measured_span = create_test_span(true);
        measured_span
            .attributes
            .insert(MEASURED_KEY.into(), AttributeValue::Float(1.0));
        let not_measured_span = create_test_span(true);
        assert!(is_measured(&measured_span));
        assert!(!is_measured(&not_measured_span));
    }

    #[test]
    fn test_is_partial_snapshot() {
        let mut partial_span = create_test_span(false);
        partial_span
            .attributes
            .insert(PARTIAL_VERSION_KEY.into(), AttributeValue::Int(2));
        let not_partial_span = create_test_span(false);
        assert!(is_partial_snapshot(&partial_span));
        assert!(!is_partial_snapshot(&not_partial_span));
    }

    #[test]
    fn test_compute_top_level() {
        let mut span_with_different_service = create_test_span_with_ids(5, 2);
        span_with_different_service.service = "another_service".into();
        let mut trace = vec![
            // Root span, should be marked as top-level
            create_test_span_with_ids(1, 0),
            // Should not be marked as top-level
            create_test_span_with_ids(2, 1),
            // No parent in local trace, should be marked as top-level
            create_test_span_with_ids(4, 3),
            // Parent belongs to another service, should be marked as top-level
            span_with_different_service,
        ];

        compute_top_level_span(trace.as_mut_slice());

        let spans_marked_as_top_level: Vec<u64> = trace
            .iter()
            .filter_map(|span| has_top_level(span).then_some(span.span_id))
            .collect();
        assert_eq!(spans_marked_as_top_level, [1, 4, 5]);
    }

    #[test]
    fn test_get_root_span_index_from_complete_trace() {
        let trace = vec![
            create_test_span_with_ids(1, 0),
            create_test_span_with_ids(2, 1),
            create_test_span_with_ids(3, 1),
        ];
        assert_eq!(get_root_span_index(&trace).unwrap(), 0);
    }

    #[test]
    fn test_get_root_span_index_root_last() {
        let trace = vec![
            create_test_span_with_ids(2, 1),
            create_test_span_with_ids(3, 1),
            create_test_span_with_ids(1, 0),
        ];
        assert_eq!(get_root_span_index(&trace).unwrap(), 2);
    }

    #[test]
    fn test_get_root_span_index_from_partial_trace() {
        // No span has parent_id == 0, but span 1's parent (99) isn't in the trace.
        let trace = vec![
            create_test_span_with_ids(1, 99),
            create_test_span_with_ids(2, 1),
        ];
        assert_eq!(get_root_span_index(&trace).unwrap(), 0);
    }

    #[test]
    fn test_get_root_span_index_empty_trace_errors() {
        let trace: Vec<SpanBytes> = vec![];
        assert!(get_root_span_index(&trace).is_err());
    }

    fn chunk_with_spans(priority: Option<i32>, spans: Vec<SpanBytes>) -> TraceChunkBytes {
        TraceChunkBytes {
            priority,
            spans,
            ..Default::default()
        }
    }

    #[test]
    fn test_drop_chunks() {
        let chunk_with_priority = chunk_with_spans(
            Some(1),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    ..Default::default()
                },
            ],
        );
        let chunk_with_null_priority = chunk_with_spans(
            Some(0),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    ..Default::default()
                },
            ],
        );
        let chunk_without_priority = chunk_with_spans(
            None,
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    ..Default::default()
                },
            ],
        );
        let chunk_with_negative_priority = chunk_with_spans(
            Some(-1),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    ..Default::default()
                },
            ],
        );
        let chunk_with_error = chunk_with_spans(
            Some(0),
            vec![
                SpanBytes {
                    span_id: 1,
                    error: true,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    ..Default::default()
                },
            ],
        );
        let chunk_with_a_single_span = chunk_with_spans(
            Some(0),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    attributes: vec![(
                        SAMPLING_SINGLE_SPAN_MECHANISM.into(),
                        AttributeValue::Float(8.0),
                    )]
                    .into(),
                    ..Default::default()
                },
            ],
        );
        // v1 tracers may send the mechanism as a native integer.
        let chunk_with_a_single_span_int_mechanism = chunk_with_spans(
            Some(0),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    attributes: vec![(
                        SAMPLING_SINGLE_SPAN_MECHANISM.into(),
                        AttributeValue::Int(8),
                    )]
                    .into(),
                    ..Default::default()
                },
            ],
        );
        let chunk_with_analyzed_span = chunk_with_spans(
            Some(0),
            vec![
                SpanBytes {
                    span_id: 1,
                    ..Default::default()
                },
                SpanBytes {
                    span_id: 2,
                    parent_id: 1,
                    attributes: vec![(
                        SAMPLING_ANALYTICS_RATE_KEY.into(),
                        AttributeValue::Float(1.0),
                    )]
                    .into(),
                    ..Default::default()
                },
            ],
        );

        let chunks_and_expected_sampled_spans = vec![
            (chunk_with_priority, 2),
            (chunk_with_null_priority, 0),
            (chunk_without_priority, 2),
            (chunk_with_negative_priority, 0),
            (chunk_with_error, 2),
            (chunk_with_a_single_span, 1),
            (chunk_with_a_single_span_int_mechanism, 1),
            (chunk_with_analyzed_span, 1),
        ];

        for (chunk, expected_count) in chunks_and_expected_sampled_spans.into_iter() {
            let mut traces = PooledTraceChunks::unpooled(vec![chunk]);
            drop_chunks(&mut traces);

            if expected_count == 0 {
                assert!(traces.is_empty());
            } else {
                assert_eq!(traces[0].spans.len(), expected_count);
            }
        }
    }

    #[test]
    fn test_drop_chunks_dropped_trace_overrides_missing_or_positive_priority() {
        // `dropped_trace: true` should reject the chunk even though `priority` alone (absent or
        // positive) would normally keep it.
        let mut dropped_without_priority = chunk_with_spans(
            None,
            vec![SpanBytes {
                span_id: 1,
                ..Default::default()
            }],
        );
        dropped_without_priority.dropped_trace = true;

        let mut dropped_with_positive_priority = chunk_with_spans(
            Some(1),
            vec![SpanBytes {
                span_id: 1,
                ..Default::default()
            }],
        );
        dropped_with_positive_priority.dropped_trace = true;

        for chunk in [dropped_without_priority, dropped_with_positive_priority] {
            let mut traces = PooledTraceChunks::unpooled(vec![chunk]);
            drop_chunks(&mut traces);
            assert!(
                traces.is_empty(),
                "dropped_trace should reject the chunk regardless of priority"
            );
        }
    }

    #[test]
    fn test_drop_chunks_dropped_trace_keeps_existing_negative_priority() {
        // dropped_trace shouldn't change behavior when the chunk already has a negative
        // priority: the chunk is still rejected, same as without dropped_trace.
        let mut chunk = chunk_with_spans(
            Some(-5),
            vec![SpanBytes {
                span_id: 1,
                ..Default::default()
            }],
        );
        chunk.dropped_trace = true;

        let mut traces = PooledTraceChunks::unpooled(vec![chunk]);
        drop_chunks(&mut traces);
        assert!(traces.is_empty());
    }
}
