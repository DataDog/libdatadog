// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace-utils functionalities implementation for tinybytes based spans

use std::collections::HashMap;
use tinybytes::BytesString;

use super::Span;

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";

fn set_top_level_span(span: &mut Span, is_top_level: bool) {
    if !is_top_level {
        if span.metrics.contains_key(TOP_LEVEL_KEY) {
            span.metrics.remove(TOP_LEVEL_KEY);
        }
        return;
    }
    span.metrics.insert(TOP_LEVEL_KEY.into(), 1.0);
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span(trace: &mut [Span]) {
    let mut span_id_to_service: HashMap<u64, BytesString> = HashMap::new();
    for span in trace.iter() {
        span_id_to_service.insert(span.span_id, span.service.clone());
    }
    for span in trace.iter_mut() {
        if span.parent_id == 0 {
            set_top_level_span(span, true);
            continue;
        }
        match span_id_to_service.get(&span.parent_id) {
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
pub fn has_top_level(span: &Span) -> bool {
    span.metrics
        .get(TRACER_TOP_LEVEL_KEY)
        .is_some_and(|v| *v == 1.0)
        || span.metrics.get(TOP_LEVEL_KEY).is_some_and(|v| *v == 1.0)
}

// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured(span: &Span) -> bool {
    span.metrics.get(MEASURED_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has a metric _dd.partial_version which is a positive
/// integer. The metric usually increases each time a new version of the same span is sent by
/// the tracer
pub fn is_partial_snapshot(span: &Span) -> bool {
    span.metrics
        .get(PARTIAL_VERSION_KEY)
        .is_some_and(|v| *v >= 0.0)
}
