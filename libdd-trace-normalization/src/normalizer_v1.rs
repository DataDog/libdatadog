// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Normalization of spans and trace chunks through the [`libdd_trace_model`] traits, mirroring the
//! agent's `normalizeV1` and `normalizeTraceChunkV1` (`pkg/trace/agent/normalizer.go`). Senders
//! that bypass the agent (agentless) apply it in the agent's place.
//!
//! The V1 protocol encodes the start and the duration of a span as `u64`, and the trait gives
//! them as `i64`. The rules here give the agent's result for values up to `i64::MAX`, see
//! [`libdd_trace_model::Span::start`] for the values above it.

use crate::normalize_utils::{self, NormalizeError};
use crate::normalizer::is_valid_http_status_code;
use libdd_trace_model::{
    AttributeRef, AttributeValue, Attributes as _, AttributesMut as _, Span as _, SpanMut,
    TraceChunk, TraceChunkMut, TraceText, ValueTypes,
};
use std::collections::HashSet;
use std::hash::BuildHasher;

const PEER_SERVICE_KEY: &str = "peer.service";
const BASE_SERVICE_KEY: &str = "_dd.base_service";
const HTTP_STATUS_CODE_KEY: &str = "http.status_code";
const LINK_NAME_KEY: &str = "link.name";

/// Returns `value` normalized by `normalize` if it changes it, and the reason `normalize` returns.
///
/// `is_normalized` is a check that doesn't allocate, such as the agent's `isNormalizedASCIITag`:
/// if it returns `true`, `value` isn't copied. Otherwise, `value` is copied and normalized, and the
/// result is compared with `value`.
fn repaired(
    value: &str,
    is_normalized: impl FnOnce(&str) -> bool,
    normalize: impl FnOnce(&mut String) -> Option<NormalizeError>,
) -> (Option<String>, Option<NormalizeError>) {
    if is_normalized(value) {
        return (None, None);
    }
    let mut s = value.to_owned();
    let reason = normalize(&mut s);
    ((s != value).then_some(s), reason)
}

/// Whether `value`, formatted as [`AttributeRef::format_as_string`] does, is a valid HTTP status
/// code. Only strings and bytes are formatted, so a number doesn't allocate.
fn is_valid_http_status_code_value<T: ValueTypes>(value: AttributeRef<'_, T>) -> bool {
    match value {
        // A negative integer is formatted with a `-`, which is invalid.
        AttributeRef::Int(i) => (100..600).contains(&i),
        // A float is formatted without a fraction only if it is an integer, and `-0` is invalid.
        AttributeRef::Float(f) => f.fract() == 0.0 && (100.0..600.0).contains(&f),
        _ => is_valid_http_status_code(&value.format_as_string()),
    }
}

/// Why the agent drops a trace chunk. [`as_str`](Self::as_str) returns the agent's `reason` tag
/// value of `datadog.trace_agent.normalizer.traces_dropped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// The chunk has no span.
    EmptyTrace,
    /// The trace ID of the chunk is zero, on all 128 bits.
    TraceIdZero,
    /// A span ID is zero.
    SpanIdZero,
}

impl DropReason {
    /// Every reason.
    pub const ALL: [Self; 3] = [Self::EmptyTrace, Self::TraceIdZero, Self::SpanIdZero];

    /// The agent's `reason` tag value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyTrace => "empty_trace",
            Self::TraceIdZero => "trace_id_zero",
            Self::SpanIdZero => "span_id_zero",
        }
    }
}

impl std::fmt::Display for DropReason {
    /// The agent's error message, without the span.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyTrace => "trace is empty (reason:empty_trace)",
            Self::TraceIdZero => "TraceID is zero (reason:trace_id_zero)",
            Self::SpanIdZero => "SpanID is zero (reason:span_id_zero)",
        })
    }
}

impl std::error::Error for DropReason {}

/// Why the agent repairs a span. [`as_str`](Self::as_str) returns the agent's `reason` tag value
/// of `datadog.trace_agent.normalizer.spans_malformed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MalformedReason {
    /// The service is empty, and becomes the fallback service.
    ServiceEmpty,
    /// The service is truncated.
    ServiceTruncate,
    /// Nothing valid is left of the service, and it becomes the fallback service.
    ServiceInvalid,
    /// `peer.service` is truncated.
    PeerServiceTruncate,
    /// Nothing valid is left of `peer.service`, and it becomes empty.
    PeerServiceInvalid,
    /// `_dd.base_service` is truncated.
    BaseServiceTruncate,
    /// Nothing valid is left of `_dd.base_service`, and it becomes empty.
    BaseServiceInvalid,
    /// The name is empty, and becomes `unnamed_operation`.
    SpanNameEmpty,
    /// The name is truncated.
    SpanNameTruncate,
    /// The name has no ASCII letter, and becomes `unnamed_operation`.
    SpanNameInvalid,
    /// The resource is empty, and becomes the name.
    ResourceEmpty,
    /// The type is truncated.
    TypeTruncate,
    /// The start is before the year 2000, and becomes now minus the duration.
    InvalidStartDate,
    /// The duration is negative, or `start + duration` overflows, and it becomes 0.
    InvalidDuration,
    /// `http.status_code` isn't between 100 and 599, and is removed.
    InvalidHttpStatusCode,
    /// An earlier span of the chunk has the same span ID, see [`count_duplicate_span_ids`].
    DuplicateSpanId,
}

impl MalformedReason {
    /// Every reason.
    pub const ALL: [Self; 16] = [
        Self::ServiceEmpty,
        Self::ServiceTruncate,
        Self::ServiceInvalid,
        Self::PeerServiceTruncate,
        Self::PeerServiceInvalid,
        Self::BaseServiceTruncate,
        Self::BaseServiceInvalid,
        Self::SpanNameEmpty,
        Self::SpanNameTruncate,
        Self::SpanNameInvalid,
        Self::ResourceEmpty,
        Self::TypeTruncate,
        Self::InvalidStartDate,
        Self::InvalidDuration,
        Self::InvalidHttpStatusCode,
        Self::DuplicateSpanId,
    ];

    /// The agent's `reason` tag value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceEmpty => "service_empty",
            Self::ServiceTruncate => "service_truncate",
            Self::ServiceInvalid => "service_invalid",
            Self::PeerServiceTruncate => "peer_service_truncate",
            Self::PeerServiceInvalid => "peer_service_invalid",
            Self::BaseServiceTruncate => "base_service_truncate",
            Self::BaseServiceInvalid => "base_service_invalid",
            Self::SpanNameEmpty => "span_name_empty",
            Self::SpanNameTruncate => "span_name_truncate",
            Self::SpanNameInvalid => "span_name_invalid",
            Self::ResourceEmpty => "resource_empty",
            Self::TypeTruncate => "type_truncate",
            Self::InvalidStartDate => "invalid_start_date",
            Self::InvalidDuration => "invalid_duration",
            Self::InvalidHttpStatusCode => "invalid_http_status_code",
            Self::DuplicateSpanId => "duplicate_span_id",
        }
    }
}

impl std::fmt::Display for MalformedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Options of [`normalize_span_with`] and [`normalize_chunk_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizeOptions<'a> {
    /// The tracer language, which names the fallback service `unnamed-<lang>-service`. If empty,
    /// the fallback service is `unnamed-service`.
    pub lang: &'a str,
    /// Whether a non-empty `component` replaces the span name before the name is normalized, as
    /// the agent's `component2name` feature does.
    pub component2name: bool,
}

/// Receives each repair that normalization makes, for telemetry such as the agent's
/// `datadog.trace_agent.normalizer.spans_malformed` count.
pub trait NormalizeObserver {
    /// Called once for each repair of a span, in the order the agent makes them.
    fn malformed(&mut self, reason: MalformedReason);
}

/// Ignores every repair.
impl NormalizeObserver for () {
    fn malformed(&mut self, _: MalformedReason) {}
}

/// Normalizes `span` in place. `lang` names the fallback service, `unnamed-<lang>-service`.
///
/// Fails if the span ID is zero: the agent drops the chunk of such a span. See
/// [`normalize_span_with`] for the options and the telemetry.
pub fn normalize_span<S: SpanMut>(span: &mut S, lang: &str) -> anyhow::Result<()> {
    let opts = NormalizeOptions {
        lang,
        ..Default::default()
    };
    Ok(normalize_span_with(span, &opts, &mut ())?)
}

/// Normalizes `span` in place, as the agent's `normalizeV1` does, and reports each repair to
/// `obs`.
///
/// Fails with [`DropReason::SpanIdZero`] if the span ID is zero, before any change: the agent
/// drops the chunk of such a span. Otherwise, in this order:
///
/// 1. The service is normalized as a tag value, with the fallback of `opts.lang`.
/// 2. `peer.service` and `_dd.base_service`, if they are non-empty strings, are normalized as tag
///    values. A value with nothing valid left becomes empty.
/// 3. With `opts.component2name`, a non-empty `component` replaces the name.
/// 4. The name is normalized. An empty resource becomes the name. A parent ID equal to the span ID
///    becomes 0.
/// 5. An invalid duration becomes 0, then a start before the year 2000 becomes now minus the
///    duration.
/// 6. The type is truncated. A non-empty env is normalized as a tag value.
/// 7. An `http.status_code` of any type that isn't between 100 and 599 is removed.
/// 8. `link.name` of any type on the span links is normalized as a name, and written as a string.
pub fn normalize_span_with<S: SpanMut>(
    span: &mut S,
    opts: &NormalizeOptions<'_>,
    obs: &mut impl NormalizeObserver,
) -> Result<(), DropReason> {
    if span.span_id() == 0 {
        return Err(DropReason::SpanIdZero);
    }

    let (service, reason) = repaired(
        span.service(),
        normalize_utils::is_normalized_service,
        |s| normalize_utils::normalize_service_with_reason(s, opts.lang),
    );
    match reason {
        Some(NormalizeError::Empty) => obs.malformed(MalformedReason::ServiceEmpty),
        Some(NormalizeError::TooLong) => obs.malformed(MalformedReason::ServiceTruncate),
        Some(NormalizeError::Invalid) => obs.malformed(MalformedReason::ServiceInvalid),
        None => {}
    }
    if let Some(v) = service {
        span.set_service(v.into());
    }

    for (key, truncate, invalid) in [
        (
            PEER_SERVICE_KEY,
            MalformedReason::PeerServiceTruncate,
            MalformedReason::PeerServiceInvalid,
        ),
        (
            BASE_SERVICE_KEY,
            MalformedReason::BaseServiceTruncate,
            MalformedReason::BaseServiceInvalid,
        ),
    ] {
        // The agent only reads a string here. An empty value is skipped, and is already
        // normalized.
        let Some((value, reason)) = span.attribute_str(key).map(|v| {
            repaired(
                v,
                normalize_utils::is_normalized_peer_service,
                normalize_utils::normalize_peer_service_with_reason,
            )
        }) else {
            continue;
        };
        match reason {
            Some(NormalizeError::TooLong) => obs.malformed(truncate),
            Some(NormalizeError::Invalid) => obs.malformed(invalid),
            Some(NormalizeError::Empty) | None => {}
        }
        if let Some(v) = value {
            span.set_attribute(
                TraceText::from_static(key),
                AttributeValue::String(v.into()),
            );
        }
    }

    if opts.component2name
        && let Some(component) = span
            .component()
            .filter(|c| !c.is_empty() && *c != span.name())
    {
        let component = component.to_owned();
        span.set_name(component.into());
    }

    let (name, reason) = repaired(
        span.name(),
        normalize_utils::is_normalized_name,
        normalize_utils::normalize_name_with_reason,
    );
    match reason {
        Some(NormalizeError::Empty) => obs.malformed(MalformedReason::SpanNameEmpty),
        Some(NormalizeError::TooLong) => obs.malformed(MalformedReason::SpanNameTruncate),
        Some(NormalizeError::Invalid) => obs.malformed(MalformedReason::SpanNameInvalid),
        None => {}
    }
    if let Some(v) = name {
        span.set_name(v.into());
    }
    if span.resource().is_empty() {
        obs.malformed(MalformedReason::ResourceEmpty);
        let name = span.name().to_owned();
        span.set_resource(name.into());
    }
    if span.parent_id() == span.span_id() {
        span.set_parent_id(0);
    }

    // The agent's `validateAndFixDurationV1` and `validateAndFixStartTimeV1` work on `u64`
    // values, and the trait gives `i64` values. For a start and a duration up to `i64::MAX`, the
    // `i64` rules give the same result: a duration that makes `start + duration` overflow
    // `i64::MAX` becomes 0, and a start before the year 2000 becomes `now - duration`, or `now` if
    // that is negative. A V1 duration above `i64::MAX` is a negative `i64` once bit-cast, which
    // also becomes 0, as in the agent. A V1 start above `i64::MAX` doesn't fit (see
    // `Span::start`): saturated to `i64::MAX`, its non-zero duration becomes 0, where the agent
    // keeps it.
    //
    // As in the agent, the duration is fixed first, and the start fix uses the fixed duration.
    if let Some(duration) = normalize_utils::fixed_duration(span.start(), span.duration()) {
        obs.malformed(MalformedReason::InvalidDuration);
        span.set_duration(duration);
    }
    if let Some(start) = normalize_utils::fixed_start(span.start(), span.duration()) {
        obs.malformed(MalformedReason::InvalidStartDate);
        span.set_start(start);
    }

    let (span_type, reason) = repaired(
        span.typ(),
        normalize_utils::is_normalized_span_type,
        normalize_utils::normalize_span_type_with_reason,
    );
    if reason.is_some() {
        obs.malformed(MalformedReason::TypeTruncate);
    }
    if let Some(v) = span_type {
        span.set_type(v.into());
    }
    // An empty env is skipped, and is already normalized.
    if let Some((Some(v), _)) = span.env().map(|v| {
        repaired(v, normalize_utils::is_normalized_tag_value, |s| {
            normalize_utils::normalize_tag_value(s);
            None
        })
    }) {
        span.set_env(v.into());
    }

    // The agent reads both attributes with `GetAttributeAsString`, which formats a value of any
    // type (see `AttributeRef::format_as_string`).
    if span
        .attribute(HTTP_STATUS_CODE_KEY)
        .is_some_and(|code| !is_valid_http_status_code_value(code))
    {
        obs.malformed(MalformedReason::InvalidHttpStatusCode);
        span.remove_attribute(HTTP_STATUS_CODE_KEY);
    }

    // The agent doesn't count the repairs of link names.
    for link in span.span_links_mut() {
        let repaired = link.attribute(LINK_NAME_KEY).and_then(|value| {
            if value
                .as_str()
                .is_some_and(normalize_utils::is_normalized_name)
            {
                return None;
            }
            let mut name = value.format_as_string().into_owned();
            normalize_utils::normalize_name(&mut name);
            // As the agent, write the name back as a string, whatever the type of the value.
            (value.as_str() != Some(name.as_str())).then_some(name)
        });
        if let Some(name) = repaired
            && let Some(value) = link.attribute_mut(LINK_NAME_KEY)
        {
            *value = AttributeValue::String(name.into());
        }
    }
    Ok(())
}

/// Normalizes every span of `chunk` in place, see [`normalize_span`].
///
/// Fails, meaning the agent would drop the chunk, if it is empty, its trace ID is zero, or a span
/// fails to normalize. See [`normalize_chunk_with`] for the options and the telemetry.
pub fn normalize_chunk<C: TraceChunkMut>(chunk: &mut C, lang: &str) -> anyhow::Result<()> {
    let opts = NormalizeOptions {
        lang,
        ..Default::default()
    };
    Ok(normalize_chunk_with(chunk, &opts, &mut ())?)
}

/// Normalizes every span of `chunk` in place, as the agent's `normalizeTraceChunkV1` does, and
/// reports each repair to `obs`. See [`normalize_span_with`].
///
/// Fails with the reason the agent drops the chunk: [`DropReason::EmptyTrace`] if it has no span,
/// [`DropReason::TraceIdZero`] if its trace ID is zero, on all 128 bits, or
/// [`DropReason::SpanIdZero`] if a span ID is zero. In the last case, as in the agent, the spans
/// before that span are normalized, and their repairs are reported.
///
/// It doesn't report [`MalformedReason::DuplicateSpanId`]: call [`count_duplicate_span_ids`] for
/// that.
pub fn normalize_chunk_with<C: TraceChunkMut>(
    chunk: &mut C,
    opts: &NormalizeOptions<'_>,
    obs: &mut impl NormalizeObserver,
) -> Result<(), DropReason> {
    if chunk.spans().next().is_none() {
        return Err(DropReason::EmptyTrace);
    }
    if chunk.trace_id() == 0 {
        return Err(DropReason::TraceIdZero);
    }
    for span in chunk.spans_mut() {
        normalize_span_with(span, opts, obs)?;
    }
    Ok(())
}

/// Reports [`MalformedReason::DuplicateSpanId`] for each span whose ID an earlier span of `chunk`
/// already has. It stops at the first span with a zero ID, so that, called after
/// [`normalize_chunk_with`] returns `Ok` or [`DropReason::SpanIdZero`], it counts the same spans as
/// the agent. `seen` is cleared first, so callers can reuse it.
///
/// Normalization doesn't change span IDs, so it can run before or after [`normalize_chunk_with`].
pub fn count_duplicate_span_ids<C: TraceChunk, H: BuildHasher>(
    chunk: &C,
    seen: &mut HashSet<u64, H>,
    obs: &mut impl NormalizeObserver,
) {
    seen.clear();
    for span in chunk.spans() {
        let id = span.span_id();
        if id == 0 {
            break;
        }
        if !seen.insert(id) {
            obs.malformed(MalformedReason::DuplicateSpanId);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_types::*;
    use libdd_trace_model::Attributes;
    use std::collections::HashMap;

    /// 2023-11-14 in nanoseconds since the epoch.
    const VALID_START: i64 = 1_700_000_000_000_000_000;

    fn valid_span() -> TestSpan {
        TestSpan {
            service: "svc".to_owned(),
            name: "op".to_owned(),
            resource: "res".to_owned(),
            typ: "web".to_owned(),
            span_id: 2,
            parent_id: 1,
            start: VALID_START,
            duration: 10,
            env: Some("prod".to_owned()),
            ..Default::default()
        }
    }

    fn normalize(mut span: TestSpan) -> TestSpan {
        normalize_span(&mut span, "php").unwrap();
        span
    }

    #[test]
    fn valid_span_is_unchanged() {
        let span = normalize(valid_span());
        assert_eq!(span.service, "svc");
        assert_eq!(span.name, "op");
        assert_eq!(span.resource, "res");
        assert_eq!(span.typ, "web");
        assert_eq!(span.parent_id, 1);
        assert_eq!((span.start, span.duration), (VALID_START, 10));
        assert_eq!(span.env.as_deref(), Some("prod"));
    }

    #[test]
    fn zero_span_id_fails() {
        let mut span = TestSpan {
            span_id: 0,
            ..valid_span()
        };
        assert!(normalize_span(&mut span, "php").is_err());
    }

    #[test]
    fn service() {
        let empty = || TestSpan {
            service: String::new(),
            ..valid_span()
        };
        assert_eq!(normalize(empty()).service, "unnamed-php-service");
        let mut span = empty();
        normalize_span(&mut span, "").unwrap();
        assert_eq!(span.service, "unnamed-service");

        let invalid = TestSpan {
            service: "My Service".to_owned(),
            ..valid_span()
        };
        assert_eq!(normalize(invalid).service, "my_service");
        let long = TestSpan {
            service: "a".repeat(150),
            ..valid_span()
        };
        assert_eq!(normalize(long).service, "a".repeat(100));
    }

    #[test]
    fn peer_and_base_service() {
        let mut span = valid_span();
        for key in [PEER_SERVICE_KEY, BASE_SERVICE_KEY] {
            span.attrs
                .insert(key.to_owned(), Value::String("Other Service".to_owned()));
        }
        let span = normalize(span);
        assert_eq!(span.attribute_str(PEER_SERVICE_KEY), Some("other_service"));
        assert_eq!(span.attribute_str(BASE_SERVICE_KEY), Some("other_service"));
    }

    #[test]
    fn services_are_normalized_as_tag_values() {
        // TestNormalizeServiceTagV1 in the agent's normalizer_test.go.
        let mut span = TestSpan {
            service: "retargeting(api-Staging ".to_owned(),
            ..valid_span()
        };
        span.attrs.insert(
            PEER_SERVICE_KEY.to_owned(),
            Value::String("retargeting(api-Peer ".to_owned()),
        );
        span.attrs.insert(
            BASE_SERVICE_KEY.to_owned(),
            Value::String("retargeting(api-Base ".to_owned()),
        );
        let span = normalize(span);
        assert_eq!(span.service, "retargeting_api-staging");
        assert_eq!(
            span.attribute_str(PEER_SERVICE_KEY),
            Some("retargeting_api-peer")
        );
        assert_eq!(
            span.attribute_str(BASE_SERVICE_KEY),
            Some("retargeting_api-base")
        );

        // Leading digits are kept, and an invalid peer service becomes empty.
        let mut span = TestSpan {
            service: "1password".to_owned(),
            ..valid_span()
        };
        span.attrs.insert(
            PEER_SERVICE_KEY.to_owned(),
            Value::String("127.0.0.1".to_owned()),
        );
        span.attrs
            .insert(BASE_SERVICE_KEY.to_owned(), Value::String("!!!".to_owned()));
        let span = normalize(span);
        assert_eq!(span.service, "1password");
        assert_eq!(span.attribute_str(PEER_SERVICE_KEY), Some("127.0.0.1"));
        assert_eq!(span.attribute_str(BASE_SERVICE_KEY), Some(""));
    }

    #[test]
    fn name_and_resource() {
        let span = normalize(TestSpan {
            name: String::new(),
            resource: String::new(),
            ..valid_span()
        });
        assert_eq!(span.name, "unnamed_operation");
        assert_eq!(span.resource, "unnamed_operation");

        let span = normalize(TestSpan {
            name: "a".repeat(150),
            ..valid_span()
        });
        assert_eq!(span.name, "a".repeat(100));
        assert_eq!(span.resource, "res");
    }

    #[test]
    fn parent_id_equal_to_span_id_is_reset() {
        let span = normalize(TestSpan {
            parent_id: 2,
            ..valid_span()
        });
        assert_eq!(span.parent_id, 0);
    }

    #[test]
    fn start_and_duration() {
        let span = normalize(TestSpan {
            duration: -5,
            ..valid_span()
        });
        assert_eq!((span.start, span.duration), (VALID_START, 0));

        let span = normalize(TestSpan {
            start: i64::MAX - 1,
            duration: 10,
            ..valid_span()
        });
        assert_eq!(span.duration, 0);

        // Starts before 2000 are a unit issue: the agent replaces them with now - duration.
        let span = normalize(TestSpan {
            start: 1_000,
            ..valid_span()
        });
        assert!(span.start > VALID_START);
        assert_eq!(span.duration, 10);
    }

    #[test]
    fn start_and_duration_match_agent_u64_rules() {
        // The agent's `validateAndFixDurationV1`, on `u64` values.
        fn agent_duration(start: u64, duration: u64) -> u64 {
            if duration > (i64::MAX as u64).wrapping_sub(start) {
                0
            } else {
                duration
            }
        }
        let max = i64::MAX as u64;
        let valid = VALID_START as u64;
        let starts = [946_684_800_000_000_000, valid, max - 1, max];
        let durations = [
            0,
            1,
            10,
            max - valid,
            max - valid + 1,
            max,
            // Bit-cast to a negative `i64`.
            max + 1,
            u64::MAX,
        ];
        for start in starts {
            for duration in durations {
                let span = normalize(TestSpan {
                    start: start as i64,
                    duration: duration as i64,
                    ..valid_span()
                });
                let expected = agent_duration(start, duration);
                assert_eq!(
                    (span.start as u64, span.duration as u64),
                    (start, expected),
                    "start {start}, duration {duration}"
                );
            }
        }
    }

    #[test]
    fn span_type_is_truncated() {
        let span = normalize(TestSpan {
            typ: "t".repeat(150),
            ..valid_span()
        });
        assert_eq!(span.typ, "t".repeat(100));
    }

    #[test]
    fn env() {
        let span = normalize(TestSpan {
            env: Some("Prod Env".to_owned()),
            ..valid_span()
        });
        assert_eq!(span.env.as_deref(), Some("prod_env"));
        // TestNormalizeEnvV1 in the agent's normalizer_test.go: leading digits are kept.
        let span = normalize(TestSpan {
            env: Some("123DEVELOPMENT".to_owned()),
            ..valid_span()
        });
        assert_eq!(span.env.as_deref(), Some("123development"));
        let span = normalize(TestSpan {
            env: None,
            ..valid_span()
        });
        assert_eq!(span.env, None);
    }

    #[test]
    fn attributes_are_read_without_attribute_mut() {
        // `attribute_mut` can copy storage that copies on write: values are only read through
        // `Attributes::attribute`.
        for code in [Value::String("200".to_owned()), Value::Int(200)] {
            let mut span = valid_span();
            span.attrs.insert(HTTP_STATUS_CODE_KEY.to_owned(), code);
            span.links.push(TestLink {
                attrs: HashMap::from([(
                    LINK_NAME_KEY.to_owned(),
                    Value::String("valid_name".to_owned()),
                )]),
            });
            let before = attribute_mut_calls();
            let span = normalize(span);
            assert_eq!(attribute_mut_calls(), before);
            assert!(span.has_attribute(HTTP_STATUS_CODE_KEY));
        }
        // A repaired link name is written through `attribute_mut`.
        let mut span = valid_span();
        span.links.push(TestLink {
            attrs: HashMap::from([(LINK_NAME_KEY.to_owned(), Value::Int(1))]),
        });
        let before = attribute_mut_calls();
        let span = normalize(span);
        assert_eq!(attribute_mut_calls(), before + 1);
        assert_eq!(
            span.links[0].attrs.get(LINK_NAME_KEY),
            Some(&Value::String("unnamed_operation".to_owned()))
        );
    }

    #[test]
    fn http_status_code() {
        let cases = [
            (Value::String("200".to_owned()), true),
            (Value::Int(404), true),
            (Value::Float(503.0), true),
            (Value::String("999".to_owned()), false),
            (Value::String("abc".to_owned()), false),
            (Value::String("+200".to_owned()), false),
            (Value::Int(42), false),
            (Value::Float(200.5), false),
            (Value::Bool(true), false),
            // The agent formats any type before it validates it.
            (Value::Bytes(b"200".to_vec()), true),
            (Value::Bytes(b"+200".to_vec()), false),
            (Value::Int(-200), false),
            (Value::Float(f64::INFINITY), false),
            (Value::Array(vec![Value::Int(200)]), false),
            (
                Value::KeyValueList(HashMap::from([("a".to_owned(), Value::Int(200))])),
                false,
            ),
        ];
        for (value, kept) in cases {
            let mut span = valid_span();
            span.attrs
                .insert(HTTP_STATUS_CODE_KEY.to_owned(), value.clone());
            let span = normalize(span);
            assert_eq!(span.has_attribute(HTTP_STATUS_CODE_KEY), kept, "{value:?}");
        }
    }

    #[test]
    fn span_link_name() {
        // TestNormalizeSpanLinkNameV1 in the agent's normalizer_test.go, and values of other
        // types, which the agent formats, normalizes, and writes back as strings.
        let cases = [
            (Value::String(String::new()), "unnamed_operation"),
            (
                Value::String("!@#$%^&*()_+".to_owned()),
                "unnamed_operation",
            ),
            (Value::String("valid_name".to_owned()), "valid_name"),
            (Value::String("http..request!".to_owned()), "http..request"),
            (Value::Int(42), "unnamed_operation"),
            (Value::Bool(true), "true"),
            (Value::Float(1.5), "unnamed_operation"),
            (Value::Bytes(b"Foo Bar".to_vec()), "Foo_Bar"),
            (
                Value::Array(vec![Value::String("a".to_owned()), Value::Int(1)]),
                "a_1",
            ),
            (
                Value::KeyValueList(HashMap::from([("k".to_owned(), Value::Bool(false))])),
                "k_false",
            ),
        ];
        for (value, expected) in cases {
            let mut span = valid_span();
            span.links.push(TestLink {
                attrs: HashMap::from([(LINK_NAME_KEY.to_owned(), value.clone())]),
            });
            span.links.push(TestLink::default());
            let span = normalize(span);
            assert_eq!(
                span.links[0].attrs.get(LINK_NAME_KEY),
                Some(&Value::String(expected.to_owned())),
                "{value:?}"
            );
            assert!(!span.links[1].has_attribute(LINK_NAME_KEY));
        }
    }

    #[test]
    fn http_status_code_fast_path_matches_formatting() {
        let mut values: Vec<Value> = [-200, 0, 99, 100, 200, 599, 600, i64::MIN, i64::MAX]
            .into_iter()
            .map(Value::Int)
            .collect();
        values.extend(
            [
                -200.0,
                -0.0,
                0.0,
                99.0,
                99.5,
                100.0,
                200.0,
                200.5,
                599.0,
                599.0000000000001,
                599.5,
                600.0,
                1e300,
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ]
            .into_iter()
            .map(Value::Float),
        );
        for value in values {
            assert_eq!(
                is_valid_http_status_code_value((&value).into()),
                is_valid_http_status_code(&value.format_as_string()),
                "{value:?}"
            );
        }
    }

    #[test]
    fn chunk() {
        let mut chunk = TestChunk {
            trace_id: 1,
            spans: vec![
                valid_span(),
                TestSpan {
                    service: String::new(),
                    ..valid_span()
                },
            ],
            ..Default::default()
        };
        normalize_chunk(&mut chunk, "php").unwrap();
        assert_eq!(chunk.spans[1].service, "unnamed-php-service");

        chunk.trace_id = 0;
        assert!(normalize_chunk(&mut chunk, "php").is_err());
        let mut empty = TestChunk {
            trace_id: 1,
            ..Default::default()
        };
        assert!(normalize_chunk(&mut empty, "php").is_err());
        let mut zero_span_id = TestChunk {
            trace_id: 1,
            spans: vec![TestSpan {
                span_id: 0,
                ..valid_span()
            }],
            ..Default::default()
        };
        assert!(normalize_chunk(&mut zero_span_id, "php").is_err());
    }
}

/// The V1 tests of the agent's `normalizer_test.go`, with the reasons each repair reports, ported
/// from Saluki's `dd_trace_normalizer` tests.
#[cfg(test)]
mod reason_tests {
    use super::*;
    use crate::test_types::*;
    use libdd_trace_model::Attributes;
    use std::collections::HashMap;

    const TEST_SPAN_ID: u64 = 0x5eed_5eed;
    use MalformedReason as M;

    impl NormalizeObserver for Vec<MalformedReason> {
        fn malformed(&mut self, reason: MalformedReason) {
            self.push(reason);
        }
    }

    /// `newTestSpanV1` of the agent's `normalizer_test.go`, with a fixed span ID.
    fn new_test_span_v1() -> TestSpan {
        let string = |s: &str| Value::String(s.to_owned());
        TestSpan {
            service: "django".to_owned(),
            name: "django.controller".to_owned(),
            resource: "GET /some/raclette".to_owned(),
            span_id: TEST_SPAN_ID,
            parent_id: 1111,
            start: 1_448_466_874_000_000_000,
            duration: 10_000_000,
            attrs: HashMap::from([
                ("user".to_owned(), string("leo")),
                ("pool".to_owned(), string("fondue")),
                ("cheese_weight".to_owned(), Value::Float(100000.0)),
            ]),
            links: vec![TestLink {
                attrs: HashMap::from([(LINK_NAME_KEY.to_owned(), string("name"))]),
            }],
            ..Default::default()
        }
    }

    fn new_test_chunk(spans: Vec<TestSpan>) -> TestChunk {
        TestChunk {
            trace_id: 424242,
            spans,
            ..Default::default()
        }
    }

    fn normalize_v1_with(
        span: &mut TestSpan,
        lang: &str,
        component2name: bool,
    ) -> (Result<(), DropReason>, Vec<MalformedReason>) {
        let mut reasons = Vec::new();
        let opts = NormalizeOptions {
            lang,
            component2name,
        };
        let result = normalize_span_with(span, &opts, &mut reasons);
        (result, reasons)
    }

    fn normalize_v1(
        span: &mut TestSpan,
        lang: &str,
    ) -> (Result<(), DropReason>, Vec<MalformedReason>) {
        normalize_v1_with(span, lang, false)
    }

    /// `normalizeTraceChunkV1`: the chunk normalization, then the duplicate span ID count.
    fn normalize_trace_chunk_v1(
        chunk: &mut TestChunk,
    ) -> (Result<(), DropReason>, Vec<MalformedReason>) {
        let mut reasons = Vec::new();
        let result = normalize_chunk_with(chunk, &NormalizeOptions::default(), &mut reasons);
        if matches!(result, Ok(()) | Err(DropReason::SpanIdZero)) {
            count_duplicate_span_ids(chunk, &mut HashSet::new(), &mut reasons);
        }
        (result, reasons)
    }

    fn set_string_attribute(span: &mut TestSpan, key: &str, value: &str) {
        span.attrs
            .insert(key.to_owned(), Value::String(value.to_owned()));
    }

    fn now_nanos() -> i64 {
        std::time::SystemTime::UNIX_EPOCH
            .elapsed()
            .unwrap()
            .as_nanos() as i64
    }

    #[test]
    fn reason_tag_values() {
        let drops: Vec<_> = DropReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(drops, ["empty_trace", "trace_id_zero", "span_id_zero"]);
        let malformed: Vec<_> = MalformedReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            malformed,
            [
                "service_empty",
                "service_truncate",
                "service_invalid",
                "peer_service_truncate",
                "peer_service_invalid",
                "base_service_truncate",
                "base_service_invalid",
                "span_name_empty",
                "span_name_truncate",
                "span_name_invalid",
                "resource_empty",
                "type_truncate",
                "invalid_start_date",
                "invalid_duration",
                "invalid_http_status_code",
                "duplicate_span_id",
            ]
        );
    }

    #[test]
    fn wrappers_keep_the_error_messages() {
        let mut span = TestSpan {
            span_id: 0,
            ..new_test_span_v1()
        };
        assert_eq!(
            normalize_span(&mut span, "").unwrap_err().to_string(),
            "SpanID is zero (reason:span_id_zero)"
        );
        let mut chunk = new_test_chunk(Vec::new());
        assert_eq!(
            normalize_chunk(&mut chunk, "").unwrap_err().to_string(),
            "trace is empty (reason:empty_trace)"
        );
        let mut chunk = TestChunk {
            trace_id: 0,
            ..new_test_chunk(vec![new_test_span_v1()])
        };
        assert_eq!(
            normalize_chunk(&mut chunk, "").unwrap_err().to_string(),
            "TraceID is zero (reason:trace_id_zero)"
        );
    }

    #[test]
    fn test_normalize_service_pass_thru_v1() {
        let mut s = new_test_span_v1();
        s.service = "foo".to_owned();
        set_string_attribute(&mut s, PEER_SERVICE_KEY, "foo");
        set_string_attribute(&mut s, BASE_SERVICE_KEY, "bar");
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.service, "foo");
        assert_eq!(s.attribute_str(PEER_SERVICE_KEY), Some("foo"));
        assert_eq!(s.attribute_str(BASE_SERVICE_KEY), Some("bar"));
    }

    #[test]
    fn test_normalize_empty_service_no_lang_v1() {
        let mut s = new_test_span_v1();
        s.service = String::new();
        set_string_attribute(&mut s, PEER_SERVICE_KEY, "");
        set_string_attribute(&mut s, BASE_SERVICE_KEY, "");
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::ServiceEmpty]));
        assert_eq!(s.service, "unnamed-service");
        // No fallback on the peer and base service tags.
        assert_eq!(s.attribute_str(PEER_SERVICE_KEY), Some(""));
        assert_eq!(s.attribute_str(BASE_SERVICE_KEY), Some(""));
    }

    #[test]
    fn test_normalize_empty_service_with_lang_v1() {
        let mut s = new_test_span_v1();
        s.service = String::new();
        set_string_attribute(&mut s, PEER_SERVICE_KEY, "");
        set_string_attribute(&mut s, BASE_SERVICE_KEY, "");
        assert_eq!(
            normalize_v1(&mut s, "java"),
            (Ok(()), vec![M::ServiceEmpty])
        );
        assert_eq!(s.service, "unnamed-java-service");
        assert_eq!(s.attribute_str(PEER_SERVICE_KEY), Some(""));
        assert_eq!(s.attribute_str(BASE_SERVICE_KEY), Some(""));
    }

    #[test]
    fn test_normalize_invalid_service_v1() {
        let mut s = new_test_span_v1();
        s.service = "!!!".to_owned();
        set_string_attribute(&mut s, PEER_SERVICE_KEY, "!!!");
        set_string_attribute(&mut s, BASE_SERVICE_KEY, "@@@");
        assert_eq!(
            normalize_v1(&mut s, "go"),
            (
                Ok(()),
                vec![
                    M::ServiceInvalid,
                    M::PeerServiceInvalid,
                    M::BaseServiceInvalid
                ]
            )
        );
        assert_eq!(s.service, "unnamed-go-service");
        assert_eq!(s.attribute_str(PEER_SERVICE_KEY), Some(""));
        assert_eq!(s.attribute_str(BASE_SERVICE_KEY), Some(""));
    }

    #[test]
    fn test_normalize_non_string_peer_service_is_skipped_v1() {
        // The agent only reads `peer.service` and `_dd.base_service` as strings.
        let mut s = new_test_span_v1();
        s.attrs.insert(PEER_SERVICE_KEY.to_owned(), Value::Int(-1));
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.attrs.get(PEER_SERVICE_KEY), Some(&Value::Int(-1)));
    }

    #[test]
    fn test_normalize_long_service_v1() {
        let mut s = new_test_span_v1();
        s.service = "CAMEMBERT".repeat(100);
        set_string_attribute(&mut s, PEER_SERVICE_KEY, &"BRIE".repeat(100));
        set_string_attribute(&mut s, BASE_SERVICE_KEY, &"ROQUEFORT".repeat(100));
        assert_eq!(
            normalize_v1(&mut s, ""),
            (
                Ok(()),
                vec![
                    M::ServiceTruncate,
                    M::PeerServiceTruncate,
                    M::BaseServiceTruncate
                ]
            )
        );
        assert_eq!(s.service, "camembert".repeat(100)[..100]);
        assert_eq!(
            s.attribute_str(PEER_SERVICE_KEY),
            Some(&"brie".repeat(100)[..100])
        );
        assert_eq!(
            s.attribute_str(BASE_SERVICE_KEY),
            Some(&"roquefort".repeat(100)[..100])
        );
    }

    #[test]
    fn test_normalize_name_pass_thru_v1() {
        let mut s = new_test_span_v1();
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.name, "django.controller");
    }

    #[test]
    fn test_normalize_empty_name_v1() {
        let mut s = TestSpan {
            name: String::new(),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::SpanNameEmpty]));
        assert_eq!(s.name, "unnamed_operation");
    }

    #[test]
    fn test_normalize_long_name_v1() {
        let mut s = TestSpan {
            name: "CAMEMBERT".repeat(100),
            ..new_test_span_v1()
        };
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Ok(()), vec![M::SpanNameTruncate])
        );
        assert_eq!(s.name, "CAMEMBERT".repeat(100)[..100]);
    }

    #[test]
    fn test_normalize_name_no_alphanumeric_v1() {
        let mut s = TestSpan {
            name: "/".to_owned(),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::SpanNameInvalid]));
        assert_eq!(s.name, "unnamed_operation");
    }

    #[test]
    fn test_normalize_resource_pass_thru_v1() {
        let mut s = new_test_span_v1();
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.resource, "GET /some/raclette");
    }

    #[test]
    fn test_normalize_empty_resource_v1() {
        let mut s = TestSpan {
            resource: String::new(),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::ResourceEmpty]));
        assert_eq!(s.resource, s.name);
    }

    #[test]
    fn test_normalize_empty_name_and_resource_v1() {
        // The resource falls back to the normalized name.
        let mut s = TestSpan {
            name: String::new(),
            resource: String::new(),
            ..new_test_span_v1()
        };
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Ok(()), vec![M::SpanNameEmpty, M::ResourceEmpty])
        );
        assert_eq!(s.resource, "unnamed_operation");
    }

    #[test]
    fn test_normalize_no_span_id_v1() {
        let mut s = TestSpan {
            span_id: 0,
            service: String::new(),
            ..new_test_span_v1()
        };
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Err(DropReason::SpanIdZero), vec![])
        );
        // Nothing changes before the span ID check.
        assert_eq!(s.service, "");
    }

    #[test]
    fn test_normalize_start_v1_pass_through() {
        let mut s = new_test_span_v1();
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.start, 1_448_466_874_000_000_000);
    }

    #[test]
    fn test_normalize_start_v1_too_small() {
        let mut s = TestSpan {
            start: 42,
            ..new_test_span_v1()
        };
        let min_start = now_nanos() - s.duration;
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Ok(()), vec![M::InvalidStartDate])
        );
        assert!(s.start >= min_start);
        assert!(s.start <= now_nanos() - s.duration);
    }

    #[test]
    fn test_normalize_start_v1_too_small_with_large_duration() {
        let mut s = TestSpan {
            start: 42,
            duration: now_nanos() * 2,
            ..new_test_span_v1()
        };
        let min_start = now_nanos();
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Ok(()), vec![M::InvalidStartDate])
        );
        assert!(s.start >= min_start, "start should have been reset to now");
        assert!(
            s.start <= now_nanos(),
            "start should have been reset to now"
        );
    }

    #[test]
    fn test_normalize_start_uses_the_fixed_duration_v1() {
        // The duration is fixed first: the start fix then uses a duration of 0.
        let mut s = TestSpan {
            start: 42,
            duration: i64::MAX,
            ..new_test_span_v1()
        };
        let min_start = now_nanos();
        assert_eq!(
            normalize_v1(&mut s, ""),
            (Ok(()), vec![M::InvalidDuration, M::InvalidStartDate])
        );
        assert_eq!(s.duration, 0);
        assert!(s.start >= min_start);
    }

    #[test]
    fn test_normalize_duration_pass_thru_v1() {
        let mut s = new_test_span_v1();
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.duration, 10_000_000);
    }

    #[test]
    fn test_normalize_empty_duration_v1() {
        let mut s = TestSpan {
            duration: 0,
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.duration, 0);
    }

    #[test]
    fn test_normalize_large_duration_v1() {
        let mut s = TestSpan {
            duration: i64::MAX,
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::InvalidDuration]));
        assert_eq!(s.duration, 0);
    }

    #[test]
    fn test_normalize_type_pass_thru_v1() {
        let mut s = TestSpan {
            typ: "http".to_owned(),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.typ, "http");
    }

    #[test]
    fn test_normalize_type_too_long_v1() {
        let mut s = TestSpan {
            typ: "sql".repeat(1000),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![M::TypeTruncate]));
        assert_eq!(s.typ.len(), normalize_utils::MAX_TYPE_LEN);
    }

    #[test]
    fn test_normalize_service_tag_v1() {
        let mut s = new_test_span_v1();
        s.service = "retargeting(api-Staging ".to_owned();
        set_string_attribute(&mut s, PEER_SERVICE_KEY, "retargeting(api-Peer ");
        set_string_attribute(&mut s, BASE_SERVICE_KEY, "retargeting(api-Base ");
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.service, "retargeting_api-staging");
        assert_eq!(
            s.attribute_str(PEER_SERVICE_KEY),
            Some("retargeting_api-peer")
        );
        assert_eq!(
            s.attribute_str(BASE_SERVICE_KEY),
            Some("retargeting_api-base")
        );
    }

    #[test]
    fn test_normalize_env_v1() {
        let mut s = TestSpan {
            env: Some("123DEVELOPMENT".to_owned()),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.env.as_deref(), Some("123development"));
    }

    #[test]
    fn test_normalize_span_link_name_v1() {
        for (link_name, expected) in [
            ("", "unnamed_operation"),
            ("!@#$%^&*()_+", "unnamed_operation"),
            ("valid_name", "valid_name"),
        ] {
            let mut s = new_test_span_v1();
            s.links[0].attrs.insert(
                LINK_NAME_KEY.to_owned(),
                Value::String(link_name.to_owned()),
            );
            // The agent doesn't count the repairs of link names.
            assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]), "{link_name:?}");
            assert_eq!(
                s.links[0].attribute_str(LINK_NAME_KEY),
                Some(expected),
                "{link_name:?}"
            );
        }
    }

    #[test]
    fn test_normalize_component2name() {
        // On, with a component. The component is then normalized as a name.
        let mut s = TestSpan {
            component: Some("component".to_owned()),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1_with(&mut s, "", true), (Ok(()), vec![]));
        assert_eq!(s.name, "component");
        let mut s = TestSpan {
            component: Some("my-component".to_owned()),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1_with(&mut s, "", true), (Ok(()), vec![]));
        assert_eq!(s.name, "my_component");

        // On, without a component.
        let mut s = new_test_span_v1();
        assert_eq!(normalize_v1_with(&mut s, "", true), (Ok(()), vec![]));
        assert_eq!(s.name, "django.controller");

        // Off.
        let mut s = TestSpan {
            component: Some("component".to_owned()),
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1_with(&mut s, "", false), (Ok(()), vec![]));
        assert_eq!(s.name, "django.controller");
    }

    #[test]
    fn test_special_zipkin_root_span() {
        // The V1 check compares the parent ID with the span ID only.
        let mut s = TestSpan {
            span_id: 42,
            parent_id: 42,
            ..new_test_span_v1()
        };
        assert_eq!(normalize_v1(&mut s, ""), (Ok(()), vec![]));
        assert_eq!(s.parent_id, 0);
        assert_eq!(s.span_id, 42);
    }

    #[test]
    fn normalize_v1_drops_invalid_http_status_code() {
        for (code, valid) in [
            (Value::String("200".to_owned()), true),
            (Value::Int(599), true),
            (Value::Float(404.0), true),
            (Value::Bytes(b"200".to_vec()), true),
            (Value::String("600".to_owned()), false),
            (Value::String("99".to_owned()), false),
            (Value::String("abc".to_owned()), false),
            (Value::String("+200".to_owned()), false),
            (Value::Int(-200), false),
            (Value::Float(200.5), false),
            (Value::Bool(true), false),
        ] {
            let mut s = new_test_span_v1();
            s.attrs
                .insert(HTTP_STATUS_CODE_KEY.to_owned(), code.clone());
            let (result, reasons) = normalize_v1(&mut s, "");
            assert_eq!(result, Ok(()));
            if valid {
                assert_eq!(s.attrs.get(HTTP_STATUS_CODE_KEY), Some(&code));
                assert_eq!(reasons, vec![], "{code:?}");
            } else {
                assert!(!s.has_attribute(HTTP_STATUS_CODE_KEY), "{code:?}");
                assert_eq!(reasons, vec![M::InvalidHttpStatusCode], "{code:?}");
            }
        }
    }

    #[test]
    fn test_normalize_trace_chunk_v1_trace_id() {
        // The trace ID is already a 128-bit value here, so only the zero check applies: the
        // agent's coercion of short and over-long IDs to 16 bytes belongs to the decoder.
        for (name, trace_id, want_drop) in [
            ("exact", 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10, false),
            ("low-64-bits-zero", 0x0102_0304_0506_0708 << 64, false),
            ("high-64-bits-zero", 1, false),
            ("all-zero", 0, true),
        ] {
            let mut chunk = TestChunk {
                trace_id,
                ..new_test_chunk(vec![new_test_span_v1()])
            };
            let (result, reasons) = normalize_trace_chunk_v1(&mut chunk);
            if want_drop {
                assert_eq!(result, Err(DropReason::TraceIdZero), "{name}");
            } else {
                assert_eq!(result, Ok(()), "{name}");
            }
            assert_eq!(reasons, vec![], "{name}");
        }
    }

    #[test]
    fn test_normalize_trace_empty() {
        let mut chunk = new_test_chunk(Vec::new());
        assert_eq!(
            normalize_trace_chunk_v1(&mut chunk),
            (Err(DropReason::EmptyTrace), vec![])
        );
    }

    #[test]
    fn test_normalize_trace_invalid_span() {
        let span2 = TestSpan {
            span_id: TEST_SPAN_ID + 1,
            name: String::new(),
            ..new_test_span_v1()
        };
        let mut chunk = new_test_chunk(vec![new_test_span_v1(), span2]);
        assert_eq!(
            normalize_trace_chunk_v1(&mut chunk),
            (Ok(()), vec![M::SpanNameEmpty])
        );
    }

    #[test]
    fn test_normalize_trace_duplicate_span_id() {
        let mut chunk = new_test_chunk(vec![new_test_span_v1(), new_test_span_v1()]);
        assert_eq!(
            normalize_trace_chunk_v1(&mut chunk),
            (Ok(()), vec![M::DuplicateSpanId])
        );
    }

    #[test]
    fn test_normalize_trace() {
        let span2 = TestSpan {
            span_id: TEST_SPAN_ID + 1,
            ..new_test_span_v1()
        };
        let mut chunk = new_test_chunk(vec![new_test_span_v1(), span2]);
        assert_eq!(normalize_trace_chunk_v1(&mut chunk), (Ok(()), vec![]));
    }

    #[test]
    fn normalize_trace_chunk_v1_counts_spans_before_the_invalid_one() {
        // A zero span ID drops the chunk, but the spans before it are normalized and counted, and
        // so are their duplicate span IDs, as in the agent. The spans after it aren't.
        let span1 = TestSpan {
            name: String::new(),
            ..new_test_span_v1()
        };
        let span3 = TestSpan {
            span_id: 0,
            ..new_test_span_v1()
        };
        let span4 = TestSpan {
            resource: String::new(),
            ..new_test_span_v1()
        };
        let mut chunk = new_test_chunk(vec![span1, new_test_span_v1(), span3, span4]);
        assert_eq!(
            normalize_trace_chunk_v1(&mut chunk),
            (
                Err(DropReason::SpanIdZero),
                vec![M::SpanNameEmpty, M::DuplicateSpanId]
            )
        );
        assert_eq!(chunk.spans[0].name, "unnamed_operation");
        assert_eq!(chunk.spans[3].resource, "");
    }

    #[test]
    fn count_duplicate_span_ids_clears_seen() {
        let chunk = new_test_chunk(vec![new_test_span_v1()]);
        let mut seen = HashSet::from([TEST_SPAN_ID]);
        let mut reasons = Vec::new();
        count_duplicate_span_ids(&chunk, &mut seen, &mut reasons);
        assert_eq!(reasons, vec![]);
        assert_eq!(seen, HashSet::from([TEST_SPAN_ID]));
    }
}
