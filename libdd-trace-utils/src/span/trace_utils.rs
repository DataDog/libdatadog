// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace-utils functionalities implementation for tinybytes based spans

use tracing::debug;

use super::{
    v04::{AttributeAnyValue, AttributeArrayValue, Span},
    SpanText, TraceData,
};
use std::collections::{HashMap, HashSet};

/// Fields whose Unicode code-point count exceeds this threshold are truncated.
pub const MAX_SPAN_STRING_LEN: usize = 25_000;
/// Length (in Unicode code points) to which over-long fields are truncated, including the suffix.
pub const TRUNCATED_SPAN_STRING_LEN: usize = 2_500;
/// Suffix appended to every truncated field.
const TRUNCATION_SUFFIX: &str = "<truncated>...";

/// Truncate all text fields in every span across all trace chunks.
///
/// Any field whose Unicode code-point count exceeds [`MAX_SPAN_STRING_LEN`] is replaced with
/// the first `TRUNCATED_SPAN_STRING_LEN - 14` code points followed by `"<truncated>..."`,
/// giving a total of [`TRUNCATED_SPAN_STRING_LEN`] code points.  Numeric fields and
/// `meta_struct` bytes are left untouched.
pub fn truncate_span_strings<T: TraceData>(traces: &mut [Vec<Span<T>>]) {
    for chunk in traces.iter_mut() {
        for span in chunk.iter_mut() {
            truncate_span(span);
        }
    }
}

fn trunc<S: SpanText>(v: S) -> S {
    v.maybe_truncate(
        MAX_SPAN_STRING_LEN,
        TRUNCATED_SPAN_STRING_LEN,
        TRUNCATION_SUFFIX,
    )
}

fn trunc_in_place<S: SpanText>(field: &mut S) {
    *field = trunc(std::mem::take(field));
}

fn truncate_attribute_value<T: TraceData>(v: AttributeAnyValue<T>) -> AttributeAnyValue<T> {
    match v {
        AttributeAnyValue::SingleValue(AttributeArrayValue::String(s)) => {
            AttributeAnyValue::SingleValue(AttributeArrayValue::String(trunc(s)))
        }
        AttributeAnyValue::Array(vec) => AttributeAnyValue::Array(
            vec.into_iter()
                .map(|item| match item {
                    AttributeArrayValue::String(s) => AttributeArrayValue::String(trunc(s)),
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

fn truncate_span<T: TraceData>(span: &mut Span<T>) {
    trunc_in_place(&mut span.service);
    trunc_in_place(&mut span.name);
    trunc_in_place(&mut span.resource);
    trunc_in_place(&mut span.r#type);

    // If truncation makes two keys identical, the downstream span.dedup() call keeps the
    // last original entry (VecMap dedup semantics). This mirrors the backend's own behavior
    // when a tracer submits a span with duplicate keys.
    for (key, value) in span.meta.iter_mut() {
        trunc_in_place(key);
        trunc_in_place(value);
    }

    for (key, _value) in span.metrics.iter_mut() {
        trunc_in_place(key);
    }

    for (key, _value) in span.meta_struct.iter_mut() {
        trunc_in_place(key);
    }

    if !span.span_links.is_empty() {
        span.span_links = std::mem::take(&mut span.span_links)
            .into_iter()
            .map(|mut link| {
                trunc_in_place(&mut link.tracestate);
                // Use entry API so that if truncation maps two originally-distinct keys to the
                // same string, the first entry's value is kept and the second is dropped without
                // allocating a truncated value for it.
                let mut new_attrs = HashMap::with_capacity(link.attributes.len());
                for (k, v) in std::mem::take(&mut link.attributes) {
                    new_attrs.entry(trunc(k)).or_insert_with(|| trunc(v));
                }
                link.attributes = new_attrs;
                link
            })
            .collect();
    }

    if !span.span_events.is_empty() {
        span.span_events = std::mem::take(&mut span.span_events)
            .into_iter()
            .map(|mut event| {
                trunc_in_place(&mut event.name);
                let mut new_attrs = HashMap::with_capacity(event.attributes.len());
                for (k, v) in std::mem::take(&mut event.attributes) {
                    new_attrs
                        .entry(trunc(k))
                        .or_insert_with(|| truncate_attribute_value(v));
                }
                event.attributes = new_attrs;
                event
            })
            .collect();
    }
}

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";

fn set_top_level_span<T>(span: &mut Span<T>)
where
    T: TraceData,
{
    span.metrics
        .insert(T::Text::from_static_str(TOP_LEVEL_KEY), 1.0);
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span<T>(trace: &mut [Span<T>])
where
    T: TraceData,
{
    let mut span_id_idx: HashMap<u64, usize> = HashMap::new();
    for (i, span) in trace.iter().enumerate() {
        span_id_idx.insert(span.span_id, i);
    }
    for span_idx in 0..trace.len() {
        let parent_id = trace[span_idx].parent_id;
        if parent_id == 0 {
            set_top_level_span(&mut trace[span_idx]);
            continue;
        }
        match span_id_idx.get(&parent_id).map(|i| &trace[*i].service) {
            Some(parent_span_service) => {
                if !(parent_span_service == &trace[span_idx].service) {
                    // parent is not in the same service
                    set_top_level_span(&mut trace[span_idx])
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(&mut trace[span_idx])
            }
        }
    }
}

pub fn get_root_span_index<T>(trace: &[Span<T>]) -> anyhow::Result<usize>
where
    T: TraceData,
{
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
                debug!(
                    trace_id = &trace[0].trace_id,
                    "trace has multiple root spans"
                );
            }
            root_span_id = Some(i);
        }
    }
    Ok(match root_span_id {
        Some(i) => i,
        None => {
            debug!(
                trace_id = &trace[0].trace_id,
                "Could not find the root span for trace"
            );
            trace.len() - 1
        }
    })
}

/// Return true if the span has a top level key set
pub fn has_top_level<T: TraceData>(span: &Span<T>) -> bool {
    span.metrics
        .get(TRACER_TOP_LEVEL_KEY)
        .is_some_and(|v| *v == 1.0)
        || span.metrics.get(TOP_LEVEL_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured<T: TraceData>(span: &Span<T>) -> bool {
    span.metrics.get(MEASURED_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has a metric _dd.partial_version which is a positive
/// integer. The metric usually increases each time a new version of the same span is sent by
/// the tracer
pub fn is_partial_snapshot<T: TraceData>(span: &Span<T>) -> bool {
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
    T: TraceData,
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
    use crate::span::v04::{
        AttributeAnyValue, AttributeArrayValue, SpanBytes, SpanEvent, SpanLink, VecMap,
    };
    use std::collections::HashMap;

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
            meta: vec![
                ("service".into(), "test-service".into()),
                ("env".into(), "test-env".into()),
                ("runtime-id".into(), "test-runtime-id-value".into()),
            ]
            .into(),
            metrics: VecMap::new(),
            r#type: "".into(),
            meta_struct: VecMap::new(),
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
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), 1.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
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
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
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
                metrics: vec![(TRACER_TOP_LEVEL_KEY.into(), 1.0)].into(),
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
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), -1.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
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
                metrics: vec![(TRACER_TOP_LEVEL_KEY.into(), 1.0)].into(),
                ..Default::default()
            },
        ];
        let chunk_with_error = vec![
            SpanBytes {
                span_id: 1,
                error: 1,
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
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
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: vec![(SAMPLING_SINGLE_SPAN_MECHANISM.into(), 8.0)].into(),
                ..Default::default()
            },
        ];
        let chunk_with_analyzed_span = vec![
            SpanBytes {
                span_id: 1,
                metrics: vec![
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]
                .into(),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: vec![(SAMPLING_ANALYTICS_RATE_KEY.into(), 1.0)].into(),
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

    // -----------------------------------------------------------------------
    // truncate_span_strings tests
    // -----------------------------------------------------------------------

    fn long_str(c: char, n: usize) -> String {
        std::iter::repeat_n(c, n).collect()
    }

    fn bs(s: &str) -> libdd_tinybytes::BytesString {
        libdd_tinybytes::BytesString::from_string(s.to_string())
    }

    fn make_span(name: &str, resource: &str, meta_key: &str, meta_val: &str) -> SpanBytes {
        SpanBytes {
            name: bs(name),
            resource: bs(resource),
            meta: vec![(bs(meta_key), bs(meta_val))].into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_no_truncation_at_limit() {
        // Exactly 25_000 chars — should NOT be truncated.
        let name = long_str('a', MAX_SPAN_STRING_LEN);
        let mut traces = vec![vec![make_span(&name, "r", "k", "v")]];
        truncate_span_strings(&mut traces);
        assert_eq!(
            traces[0][0].name.as_str().chars().count(),
            MAX_SPAN_STRING_LEN
        );
    }

    #[test]
    fn test_truncation_over_limit() {
        // 25_001 chars — should be truncated to 2_500.
        let resource = long_str('b', MAX_SPAN_STRING_LEN + 1);
        let mut traces = vec![vec![make_span("n", &resource, "k", "v")]];
        truncate_span_strings(&mut traces);
        let result = traces[0][0].resource.as_str();
        assert_eq!(result.chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert!(result.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn test_meta_key_and_value_truncated() {
        let long_key = long_str('c', MAX_SPAN_STRING_LEN + 1);
        let short_val = long_str('d', 2_000); // under limit — unchanged
        let mut traces = vec![vec![make_span("n", "r", &long_key, &short_val)]];
        truncate_span_strings(&mut traces);
        let (k, v) = traces[0][0].meta.iter().next().unwrap();
        assert_eq!(k.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert!(k.as_str().ends_with(TRUNCATION_SUFFIX));
        assert_eq!(v.as_str().chars().count(), 2_000); // unchanged
    }

    #[test]
    fn test_unicode_truncation_by_code_points() {
        // Each '€' is 3 bytes; 25_001 euros exceed the threshold.
        let s = long_str('€', MAX_SPAN_STRING_LEN + 1);
        let mut traces = vec![vec![make_span(&s, "r", "k", "v")]];
        truncate_span_strings(&mut traces);
        let result = traces[0][0].name.as_str();
        // Result must be exactly TRUNCATED_SPAN_STRING_LEN code points.
        assert_eq!(result.chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert!(result.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn test_span_link_fields_truncated() {
        let long_tracestate = long_str('x', MAX_SPAN_STRING_LEN + 1);
        let long_attr_key = long_str('y', MAX_SPAN_STRING_LEN + 1);
        let long_attr_val = long_str('z', MAX_SPAN_STRING_LEN + 1);
        let mut traces = vec![vec![SpanBytes {
            span_links: vec![SpanLink {
                tracestate: long_tracestate.into(),
                attributes: HashMap::from([(long_attr_key.into(), long_attr_val.into())]),
                ..Default::default()
            }],
            ..Default::default()
        }]];
        truncate_span_strings(&mut traces);
        let link = &traces[0][0].span_links[0];
        assert_eq!(
            link.tracestate.as_str().chars().count(),
            TRUNCATED_SPAN_STRING_LEN
        );
        let (k, v) = link.attributes.iter().next().unwrap();
        assert_eq!(k.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert_eq!(v.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
    }

    #[test]
    fn test_span_event_name_and_string_attribute_truncated() {
        let long_name = long_str('e', MAX_SPAN_STRING_LEN + 1);
        let long_str_attr = long_str('f', MAX_SPAN_STRING_LEN + 1);
        let mut traces = vec![vec![SpanBytes {
            span_events: vec![SpanEvent {
                name: long_name.into(),
                attributes: HashMap::from([
                    (
                        "str_attr".into(),
                        AttributeAnyValue::SingleValue(AttributeArrayValue::String(
                            long_str_attr.into(),
                        )),
                    ),
                    (
                        "int_attr".into(),
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(42)),
                    ),
                ]),
                ..Default::default()
            }],
            ..Default::default()
        }]];
        truncate_span_strings(&mut traces);
        let event = &traces[0][0].span_events[0];
        assert_eq!(
            event.name.as_str().chars().count(),
            TRUNCATED_SPAN_STRING_LEN
        );
        match event.attributes.get("str_attr").unwrap() {
            AttributeAnyValue::SingleValue(AttributeArrayValue::String(s)) => {
                assert_eq!(s.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
            }
            _ => panic!("expected string attribute"),
        }
        // Integer attribute untouched
        match event.attributes.get("int_attr").unwrap() {
            AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(42)) => {}
            _ => panic!("expected integer attribute"),
        }
    }

    #[test]
    fn test_metric_key_truncated() {
        let long_key = long_str('g', MAX_SPAN_STRING_LEN + 1);
        let mut traces = vec![vec![SpanBytes {
            metrics: vec![(bs(&long_key), 1.0_f64)].into(),
            ..Default::default()
        }]];
        truncate_span_strings(&mut traces);
        let (k, v) = traces[0][0].metrics.iter().next().unwrap();
        assert_eq!(k.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert!(k.as_str().ends_with(TRUNCATION_SUFFIX));
        assert_eq!(*v, 1.0_f64);
    }

    #[test]
    fn test_meta_struct_key_truncated() {
        use libdd_tinybytes::Bytes;
        let long_key = long_str('h', MAX_SPAN_STRING_LEN + 1);
        let payload = Bytes::from_static(b"some bytes");
        let mut traces = vec![vec![SpanBytes {
            meta_struct: vec![(bs(&long_key), payload)].into(),
            ..Default::default()
        }]];
        truncate_span_strings(&mut traces);
        let (k, v) = traces[0][0].meta_struct.iter().next().unwrap();
        assert_eq!(k.as_str().chars().count(), TRUNCATED_SPAN_STRING_LEN);
        assert_eq!(v.as_ref(), b"some bytes"); // value unchanged
    }
}
