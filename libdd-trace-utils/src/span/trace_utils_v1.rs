// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace-utils functionalities implementation for V1 spans.

use crate::span::trace_utils::DroppedP0Stats;
use crate::span::v1::{AttributeValue, Span, TraceChunk};
use crate::span::{SpanText, TraceData};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Span attribute the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span attribute the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";
const SAMPLING_SINGLE_SPAN_MECHANISM: &str = "_dd.span_sampling.mechanism";
const SAMPLING_ANALYTICS_RATE_KEY: &str = "_dd1.sr.eausr";

/// Reads a numeric attribute (`Float` or `Int`), mirroring how v0.4's `metrics` map is read.
fn attribute_as_f64<T: TraceData>(value: &AttributeValue<T>) -> Option<f64> {
    match value {
        AttributeValue::Float(v) => Some(*v),
        AttributeValue::Int(v) => Some(*v as f64),
        _ => None,
    }
}

fn set_top_level_span<T: TraceData>(span: &mut Span<T>) {
    span.attributes.insert(
        T::Text::from_static_str(TOP_LEVEL_KEY),
        AttributeValue::Float(1.0),
    );
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span<T: TraceData>(trace: &mut [Span<T>]) {
    let span_id_idx: HashMap<u64, usize> = trace
        .iter()
        .enumerate()
        .map(|(i, span)| (span.span_id, i))
        .collect();
    let top_level: Vec<usize> = trace
        .iter()
        .enumerate()
        .filter(|(_, span)| {
            span.parent_id == 0
                || span_id_idx
                    .get(&span.parent_id)
                    .is_none_or(|&parent_idx| trace[parent_idx].service != span.service)
        })
        .map(|(i, _)| i)
        .collect();
    for i in top_level {
        set_top_level_span(&mut trace[i]);
    }
}

/// Returns the index of the root span in `trace`.
pub fn get_root_span_index<T: TraceData>(trace: &[Span<T>]) -> anyhow::Result<usize> {
    if trace.is_empty() {
        anyhow::bail!("Cannot find root span index in an empty trace.");
    }

    // Do a first pass to find if we have an obvious root span (starting from the end) since some
    // clients put the root span last.
    for (i, span) in trace.iter().enumerate().rev() {
        if span.parent_id == 0 {
            return Ok(i);
        }
    }

    let span_ids: HashSet<_> = trace.iter().map(|span| span.span_id).collect();

    let mut root_span_id = None;
    for (i, span) in trace.iter().enumerate() {
        // If a span's parent is not in the trace, it is a root
        if !span_ids.contains(&span.parent_id) {
            if root_span_id.is_some() {
                debug!("trace has multiple root spans");
            }
            root_span_id = Some(i);
        }
    }
    Ok(match root_span_id {
        Some(i) => i,
        None => {
            debug!("Could not find the root span for trace");
            trace.len() - 1
        }
    })
}

/// Returns true if the span has a top level key set
pub fn has_top_level<T: TraceData>(span: &Span<T>) -> bool {
    span.attributes
        .get(TRACER_TOP_LEVEL_KEY)
        .and_then(attribute_as_f64)
        .is_some_and(|v| v == 1.0)
        || span
            .attributes
            .get(TOP_LEVEL_KEY)
            .and_then(attribute_as_f64)
            .is_some_and(|v| v == 1.0)
}

/// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured<T: TraceData>(span: &Span<T>) -> bool {
    span.attributes
        .get(MEASURED_KEY)
        .and_then(attribute_as_f64)
        .is_some_and(|v| v == 1.0)
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has an attribute _dd.partial_version which is a positive
/// integer. This value usually increases each time a new version of the same span is sent by
/// the tracer
pub fn is_partial_snapshot<T: TraceData>(span: &Span<T>) -> bool {
    span.attributes
        .get(PARTIAL_VERSION_KEY)
        .and_then(attribute_as_f64)
        .is_some_and(|v| v >= 0.0)
}

/// Remove spans and chunks, only keeping the ones that may be sampled by the agent.
///
/// Unlike v0.4 (where sampling priority is a per-span metric), v1's `priority` is already a
/// direct field on [`TraceChunk`], so it is read once per chunk instead of being searched for
/// across the chunk's spans.
///
/// # Returns
///
/// The number of P0 traces and spans that were dropped.
pub fn drop_chunks<T: TraceData>(traces: &mut Vec<TraceChunk<T>>) -> DroppedP0Stats {
    let mut dropped_p0_traces = 0;
    let mut dropped_p0_spans = 0;

    traces.retain_mut(|chunk| {
        // ErrorSampler
        if chunk.spans.iter().any(|s| s.error) {
            // We send chunks containing an error
            return true;
        }

        // PrioritySampler and NoPrioritySampler
        //
        // `dropped_trace` signals the trace was rejected by samplers even when `priority` is
        // absent or positive; treat it as an implicit negative priority (mirrors the
        // v1-to-v0.4 encoder's handling of this flag), unless the chunk already carries a
        // negative priority of its own.
        let effective_priority = if chunk.dropped_trace {
            chunk.priority.filter(|&p| p < 0).or(Some(-1))
        } else {
            chunk.priority
        };
        if effective_priority.is_none_or(|p| p > 0) {
            // We send chunks with positive priority or no priority
            return true;
        }

        // SingleSpanSampler and AnalyzedSpansSampler
        // Keep only the spans sampled by single-span sampling or analyzed spans, even if the
        // chunk is dropped.
        let spans_before = chunk.spans.len();
        chunk.spans.retain(|span| {
            span.attributes
                .get(SAMPLING_SINGLE_SPAN_MECHANISM)
                .and_then(attribute_as_f64)
                .is_some_and(|m| m == 8.0)
                || span.attributes.contains_key(SAMPLING_ANALYTICS_RATE_KEY)
        });
        dropped_p0_spans += spans_before - chunk.spans.len();
        if chunk.spans.is_empty() {
            // If no spans were sampled we can drop the whole chunk
            dropped_p0_traces += 1;
            return false;
        }
        true
    });

    DroppedP0Stats {
        dropped_p0_traces,
        dropped_p0_spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v1::{SpanBytes, TraceChunkBytes};

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
            (chunk_with_analyzed_span, 1),
        ];

        for (chunk, expected_count) in chunks_and_expected_sampled_spans.into_iter() {
            let mut traces = vec![chunk];
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
            let mut traces = vec![chunk];
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

        let mut traces = vec![chunk];
        drop_chunks(&mut traces);
        assert!(traces.is_empty());
    }
}
