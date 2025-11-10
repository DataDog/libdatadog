// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace-utils functionalities implementation for tinybytes based spans

use super::{Span, SpanText};
use std::collections::HashMap;

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";

fn set_top_level_span<T>(span: &mut Span<T>, is_top_level: bool)
where
    T: SpanText,
{
    if is_top_level {
        span.metrics.insert(T::from_static_str(TOP_LEVEL_KEY), 1.0);
    } else {
        span.metrics.remove(TOP_LEVEL_KEY);
    }
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span<T>(trace: &mut [Span<T>])
where
    T: SpanText,
{
    let mut span_id_idx: HashMap<u64, usize> = HashMap::new();
    for (i, span) in trace.iter().enumerate() {
        span_id_idx.insert(span.span_id, i);
    }
    for span_idx in 0..trace.len() {
        let parent_id = trace[span_idx].parent_id;
        if parent_id == 0 {
            set_top_level_span(&mut trace[span_idx], true);
            continue;
        }
        match span_id_idx.get(&parent_id).map(|i| &trace[*i].service) {
            Some(parent_span_service) => {
                if !(parent_span_service == &trace[span_idx].service) {
                    // parent is not in the same service
                    set_top_level_span(&mut trace[span_idx], true)
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(&mut trace[span_idx], true)
            }
        }
    }
}

/// Return true if the span has a top level key set
pub fn has_top_level<T: SpanText>(span: &Span<T>) -> bool {
    span.metrics
        .get(TRACER_TOP_LEVEL_KEY)
        .is_some_and(|v| *v == 1.0)
        || span.metrics.get(TOP_LEVEL_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured<T: SpanText>(span: &Span<T>) -> bool {
    span.metrics.get(MEASURED_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has a metric _dd.partial_version which is a positive
/// integer. The metric usually increases each time a new version of the same span is sent by
/// the tracer
pub fn is_partial_snapshot<T: SpanText>(span: &Span<T>) -> bool {
    span.metrics
        .get(PARTIAL_VERSION_KEY)
        .is_some_and(|v| *v >= 0.0)
}

pub struct DroppedP0Stats {
    pub dropped_p0_traces: usize,
    pub dropped_p0_spans: usize,
}

// Keys used for sampling
const SAMPLING_PRIORITY_KEY: &str = "_sampling_priority_v1";
const SAMPLING_SINGLE_SPAN_MECHANISM: &str = "_dd.span_sampling.mechanism";
const SAMPLING_ANALYTICS_RATE_KEY: &str = "_dd1.sr.eausr";

/// Remove spans and chunks from a TraceCollection only keeping the ones that may be sampled by
/// the agent.
///
/// # Returns
///
/// A tuple containing the dropped p0 stats, the first value correspond the amount of traces
/// dropped and the latter to the spans dropped.
///
/// # Trace-level attributes
/// Some attributes related to the whole trace are stored in the root span of the chunk.  
pub fn drop_chunks<T>(traces: &mut Vec<Vec<Span<T>>>) -> DroppedP0Stats
where
    T: SpanText,
{
    let mut dropped_p0_traces = 0;
    let mut dropped_p0_spans = 0;

    traces.retain_mut(|chunk| {
        // ErrorSampler
        if chunk.iter().any(|s| s.error == 1) {
            // We send chunks containing an error
            return true;
        }

        // PrioritySampler and NoPrioritySampler
        let chunk_priority = chunk
            .iter()
            .find_map(|s| s.metrics.get(SAMPLING_PRIORITY_KEY));
        if chunk_priority.is_none_or(|p| *p > 0.0) {
            // We send chunks with positive priority or no priority
            return true;
        }

        // SingleSpanSampler and AnalyzedSpansSampler
        // List of spans to keep even if the chunk is dropped
        let mut sampled_indexes = Vec::new();
        for (index, span) in chunk.iter().enumerate() {
            if span
                .metrics
                .get(SAMPLING_SINGLE_SPAN_MECHANISM)
                .is_some_and(|m| *m == 8.0)
                || span.metrics.contains_key(SAMPLING_ANALYTICS_RATE_KEY)
            {
                // We send spans sampled by single-span sampling or analyzed spans
                sampled_indexes.push(index);
            }
        }
        dropped_p0_spans += chunk.len() - sampled_indexes.len();
        if sampled_indexes.is_empty() {
            // If no spans were sampled we can drop the whole chunk
            dropped_p0_traces += 1;
            return false;
        }
        let sampled_spans = sampled_indexes
            .iter()
            .map(|i| std::mem::take(&mut chunk[*i]))
            .collect();
        *chunk = sampled_spans;
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
    use crate::span::SpanBytes;

    fn create_test_span(
        trace_id: u64,
        span_id: u64,
        parent_id: u64,
        start: i64,
        is_top_level: bool,
    ) -> SpanBytes {
        let mut span = SpanBytes {
            trace_id: trace_id as u128,
            span_id,
            service: "test-service".into(),
            name: "test_name".into(),
            resource: "test-resource".into(),
            parent_id,
            start,
            duration: 5,
            error: 0,
            meta: HashMap::from([
                ("service".into(), "test-service".into()),
                ("env".into(), "test-env".into()),
                ("runtime-id".into(), "test-runtime-id-value".into()),
            ]),
            metrics: HashMap::new(),
            r#type: "".into(),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        };
        if is_top_level {
            span.metrics.insert("_top_level".into(), 1.0);
            span.meta
                .insert("_dd.origin".into(), "cloudfunction".into());
            span.meta.insert("origin".into(), "cloudfunction".into());
            span.meta
                .insert("functionname".into(), "dummy_function_name".into());
        }
        span
    }

    #[test]
    fn test_has_top_level() {
        let top_level_span = create_test_span(123, 1234, 12, 1, true);
        let not_top_level_span = create_test_span(123, 1234, 12, 1, false);
        assert!(has_top_level(&top_level_span));
        assert!(!has_top_level(&not_top_level_span));
    }

    #[test]
    fn test_is_measured() {
        let mut measured_span = create_test_span(123, 1234, 12, 1, true);
        measured_span.metrics.insert(MEASURED_KEY.into(), 1.0);
        let not_measured_span = create_test_span(123, 1234, 12, 1, true);
        assert!(is_measured(&measured_span));
        assert!(!is_measured(&not_measured_span));
    }

    #[test]
    fn test_compute_top_level() {
        let mut span_with_different_service = create_test_span(123, 5, 2, 1, false);
        span_with_different_service.service = "another_service".into();
        let mut trace = vec![
            // Root span, should be marked as top-level
            create_test_span(123, 1, 0, 1, false),
            // Should not be marked as top-level
            create_test_span(123, 2, 1, 1, false),
            // No parent in local trace, should be marked as
            // top-level
            create_test_span(123, 4, 3, 1, false),
            // Parent belongs to another service, should be marked
            // as top-level
            span_with_different_service,
        ];

        compute_top_level_span(trace.as_mut_slice());

        let spans_marked_as_top_level: Vec<u64> = trace
            .iter()
            .filter_map(|span| {
                if has_top_level(span) {
                    Some(span.span_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(spans_marked_as_top_level, [1, 4, 5])
    }

    #[test]
    fn test_drop_chunks() {
        let chunk_with_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 1.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_null_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_without_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([(TRACER_TOP_LEVEL_KEY.into(), 1.0)]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_multiple_top_level = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), -1.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
            SpanBytes {
                span_id: 4,
                parent_id: 3,
                metrics: HashMap::from([(TRACER_TOP_LEVEL_KEY.into(), 1.0)]),
                ..Default::default()
            },
        ];
        let chunk_with_error = vec![
            SpanBytes {
                span_id: 1,
                error: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_a_single_span = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([(SAMPLING_SINGLE_SPAN_MECHANISM.into(), 8.0)]),
                ..Default::default()
            },
        ];
        let chunk_with_analyzed_span = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([(SAMPLING_ANALYTICS_RATE_KEY.into(), 1.0)]),
                ..Default::default()
            },
        ];

        let chunks_and_expected_sampled_spans = vec![
            (chunk_with_priority, 2),
            (chunk_with_null_priority, 0),
            (chunk_without_priority, 2),
            (chunk_with_multiple_top_level, 0),
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
                assert_eq!(traces[0].len(), expected_count);
            }
        }
    }
}
