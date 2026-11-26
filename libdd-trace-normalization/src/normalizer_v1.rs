// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Normalization of spans and trace chunks through the [`libdd_trace_model`] traits, mirroring the
//! agent's `normalizeV1` and `normalizeTraceChunkV1` (`pkg/trace/agent/normalizer.go`). Senders
//! that bypass the agent (agentless) apply it in the agent's place.
//!
//! The V1 protocol encodes the start and the duration of a span as `u64`, and the trait gives
//! them as `i64`. The rules here give the agent's result for values up to `i64::MAX`, see
//! [`libdd_trace_model::Span::start`] for the values above it.

use crate::normalize_utils;
use crate::normalizer::is_valid_http_status_code;
use libdd_trace_model::{
    AttributeRef, AttributeValue, Attributes as _, AttributesMut as _, SpanMut, TraceChunkMut,
    TraceText,
};

const PEER_SERVICE_KEY: &str = "peer.service";
const BASE_SERVICE_KEY: &str = "_dd.base_service";
const HTTP_STATUS_CODE_KEY: &str = "http.status_code";
const LINK_NAME_KEY: &str = "link.name";

/// Returns `value` normalized by `normalize`, or `None` if it is already normalized.
fn normalized(value: &str, normalize: impl FnOnce(&mut String)) -> Option<String> {
    let mut s = value.to_owned();
    normalize(&mut s);
    (s != value).then_some(s)
}

/// Normalizes `span` in place. `lang` names the fallback service, `unnamed-<lang>-service`.
///
/// Fails if the span ID is zero: the agent drops the chunk of such a span.
pub fn normalize_span<S: SpanMut>(span: &mut S, lang: &str) -> anyhow::Result<()> {
    anyhow::ensure!(span.span_id() != 0, "SpanID is zero (reason:span_id_zero)");

    if let Some(v) = normalized(span.service(), |s| {
        normalize_utils::normalize_service_with_lang(s, lang)
    }) {
        span.set_service(v.into());
    }
    for key in [PEER_SERVICE_KEY, BASE_SERVICE_KEY] {
        if let Some(v) = span
            .attribute_str(key)
            .filter(|v| !v.is_empty())
            .and_then(|v| normalized(v, normalize_utils::normalize_peer_service))
        {
            span.set_attribute(
                TraceText::from_static(key),
                AttributeValue::String(v.into()),
            );
        }
    }

    if let Some(v) = normalized(span.name(), normalize_utils::normalize_name) {
        span.set_name(v.into());
    }
    if span.resource().is_empty() {
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
    let (mut start, mut duration) = (span.start(), span.duration());
    normalize_utils::normalize_span_start_duration(&mut start, &mut duration);
    span.set_start(start);
    span.set_duration(duration);

    if let Some(v) = normalized(span.typ(), normalize_utils::normalize_span_type) {
        span.set_type(v.into());
    }
    if let Some(v) = span
        .env()
        .filter(|v| !v.is_empty())
        .and_then(|v| normalized(v, normalize_utils::normalize_tag_value))
    {
        span.set_env(v.into());
    }

    // The agent reads both attributes with `GetAttributeAsString`, which formats a value of any
    // type (see `AttributeRef::format_as_string`). The full formatting of an array or a map can
    // be far larger than the payload, as the V1 string table lets it reference one string many
    // times, so neither is formatted in full.
    if span
        .attribute(HTTP_STATUS_CODE_KEY)
        .is_some_and(|code| match code {
            // Formatted as `[...]` or `{...}`, which is never a valid status code.
            AttributeRef::Array(_) | AttributeRef::KeyValueList(_) => true,
            _ => !is_valid_http_status_code(&code.format_as_string()),
        })
    {
        span.remove_attribute(HTTP_STATUS_CODE_KEY);
    }

    for link in span.span_links_mut() {
        let repaired = link.attribute(LINK_NAME_KEY).and_then(|value| {
            // `normalize_name` first truncates to `MAX_NAME_LEN`, so the rest isn't needed.
            let mut name = value
                .format_as_string_truncated(normalize_utils::MAX_NAME_LEN)
                .into_owned();
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
/// fails to normalize.
pub fn normalize_chunk<C: TraceChunkMut>(chunk: &mut C, lang: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        chunk.spans().next().is_some(),
        "trace is empty (reason:empty_trace)"
    );
    anyhow::ensure!(
        chunk.trace_id() != 0,
        "TraceID is zero (reason:trace_id_zero)"
    );
    for span in chunk.spans_mut() {
        normalize_span(span, lang)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_model::{TestChunk, TestLink, TestSpan, Value, attribute_mut_calls};
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
            // Truncated to `MAX_NAME_LEN` before the leading `[` is stripped.
            (
                Value::Array(vec![Value::String("a".repeat(150)); 3]),
                &"a".repeat(99),
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
