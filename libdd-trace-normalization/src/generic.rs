// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Normalization of any span representation implementing the `libdd-trace-model` write traits.
//!
//! Follows the agent's v1 normalizer (`normalizeV1` and `normalizeTraceChunkV1` in
//! `pkg/trace/agent/normalizer.go`). The field rules are shared with the protobuf
//! [`normalizer`](crate::normalizer) through [`normalize_utils`]. Where the two differ, it is
//! because the agent's v1 normalizer differs from its v0.4 one:
//! - spans are not rejected for a zero trace id, and there is no foreign span check: the trace id
//!   belongs to the chunk;
//! - the parent id is reset when it equals the span id, whatever the trace id;
//! - `http.status_code` is validated whatever its type, not only as a string.

use crate::{normalize_utils, normalizer};
use libdd_trace_model::{Attributes, ORIGIN_KEY, SAMPLING_PRIORITY_KEY, SpanMut, TraceChunkMut};

const HTTP_STATUS_CODE_KEY: &str = "http.status_code";

/// The reason a trace chunk could not be normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    /// The chunk has no spans; it must be dropped.
    #[error("trace is empty (reason:empty_trace)")]
    EmptyTrace,
    /// A span of the chunk has a zero span id; the chunk must be dropped.
    #[error("span id is zero (reason:span_id_zero)")]
    SpanIdZero,
    /// The root span index passed to [`normalize_chunk`] is out of bounds.
    #[error("root span index {0} is out of bounds")]
    RootSpanIndexOutOfBounds(usize),
}

/// Normalizes every span of `chunk` in place.
///
/// Returns an error if the chunk can't be repaired, in which case it must be dropped (some of its
/// spans may already have been modified).
pub fn normalize_trace<C: TraceChunkMut>(chunk: &mut C) -> Result<(), NormalizeError> {
    if chunk.spans().next().is_none() {
        return Err(NormalizeError::EmptyTrace);
    }
    // Shared by every field of every span, so that values which are already normal cost no
    // allocation.
    let mut buf = String::new();
    for span in chunk.spans_mut() {
        normalize_span(span, &mut buf)?;
    }
    Ok(())
}

fn normalize_span<S: SpanMut>(span: &mut S, buf: &mut String) -> Result<(), NormalizeError> {
    if span.span_id() == 0 {
        return Err(NormalizeError::SpanIdZero);
    }

    // TODO: component2name, as in the protobuf normalizer.

    if let Some(service) = normalize_utils::normalized_service(span.service(), buf) {
        span.set_service(service.to_owned());
    }
    if let Some(name) = normalize_utils::normalized_name(span.name(), buf) {
        span.set_name(name.to_owned());
    }
    if span.resource().is_empty() {
        let name = span.name().to_owned();
        span.set_resource(name);
    }
    // The span id alone, see the module documentation.
    if span.parent_id() == span.span_id() {
        span.set_parent_id(0);
    }

    let (mut start, mut duration) = (span.start(), span.duration());
    normalize_utils::normalize_span_start_duration(&mut start, &mut duration);
    span.set_start(start);
    span.set_duration(duration);

    if let Some(span_type) = normalize_utils::normalized_span_type(span.typ(), buf) {
        span.set_type(span_type.to_owned());
    }
    if let Some(env) = span.env()
        && let Some(env) = normalize_utils::normalized_tag(env, buf)
    {
        span.set_env(env.to_owned());
    }

    if span.has_attribute(HTTP_STATUS_CODE_KEY) && !has_valid_http_status_code(span) {
        span.remove_attribute(HTTP_STATUS_CODE_KEY);
    }
    Ok(())
}

/// The agent stringifies a non-string `http.status_code` before validating it: an integer, or a
/// float holding one, is valid when in range, and any other type is invalid.
fn has_valid_http_status_code(span: &impl Attributes) -> bool {
    match span.attribute_str(HTTP_STATUS_CODE_KEY) {
        Some(code) => normalizer::is_valid_http_status_code(code),
        None => span
            .attribute_i64(HTTP_STATUS_CODE_KEY)
            .is_some_and(normalizer::is_valid_http_status_code_int),
    }
}

/// Fills the chunk fields older tracers set on spans instead, from the span at `root_span_index`:
/// * the sampling priority, if unset, from the root span's `_sampling_priority_v1`, else from the
///   first span that has one; a value that isn't an exact `i32` is ignored;
/// * the origin, if empty, from the root span's `_dd.origin`.
///
/// The agent doesn't do this for v1 payloads, but the v1 decoder doesn't promote these tags to the
/// chunk fields yet.
pub fn normalize_chunk<C: TraceChunkMut>(
    chunk: &mut C,
    root_span_index: usize,
) -> Result<(), NormalizeError> {
    let Some(root) = chunk.spans().nth(root_span_index) else {
        return Err(NormalizeError::RootSpanIndexOutOfBounds(root_span_index));
    };
    let priority = if chunk.priority().is_some() {
        None
    } else if root.has_attribute(SAMPLING_PRIORITY_KEY) {
        // Like the agent, a root priority is used even if invalid (here: not an exact `i32`).
        sampling_priority(root)
    } else {
        chunk
            .spans()
            .find_map(|span| {
                span.has_attribute(SAMPLING_PRIORITY_KEY)
                    .then(|| sampling_priority(span))
            })
            .flatten()
    };
    let origin = if chunk.origin().is_empty() {
        root.attribute_str(ORIGIN_KEY).map(str::to_owned)
    } else {
        None
    };

    if let Some(priority) = priority {
        chunk.set_priority(priority);
    }
    if let Some(origin) = origin {
        chunk.set_origin(origin);
    }
    Ok(())
}

fn sampling_priority(span: &impl Attributes) -> Option<i32> {
    i32::try_from(span.attribute_i64(SAMPLING_PRIORITY_KEY)?).ok()
}
