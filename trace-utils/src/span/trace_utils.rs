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

fn set_top_level_span<'a, T>(span: &mut Span<T>, is_top_level: bool)
where
    T: SpanText + From<&'a str>,
{
    if is_top_level {
        span.metrics.insert(TOP_LEVEL_KEY.into(), 1.0);
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
pub fn compute_top_level_span<'a, T>(trace: &mut [Span<T>])
where
    T: SpanText + Clone + From<&'a str>,
{
    let mut span_id_to_service: HashMap<u64, T> = HashMap::new();
    for span in trace.iter() {
        span_id_to_service.insert(span.span_id, span.service.clone());
    }
    for span in trace.iter_mut() {
        let parent_id = span.parent_id;
        if parent_id == 0 {
            set_top_level_span(span, true);
            continue;
        }
        match span_id_to_service.get(&parent_id) {
            Some(parent_span_service) => {
                if !parent_span_service.eq(&span.service) {
                    // parent is not in the same service
                    set_top_level_span(span, true)
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(span, true)
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
            trace_id,
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
        };
        if is_top_level {
            span.metrics.insert("_top_level".into(), 1.0);
            span.meta
                .insert("_dd.origin".into(), "cloudfunction".into());
            span.meta.insert("origin".into(), "cloudfunction".into());
            span.meta
                .insert("functionname".into(), "dummy_function_name".into());
            span.r#type = "serverless".into();
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
}
