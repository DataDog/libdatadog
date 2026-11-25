// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Normalization of spans and trace chunks through the [`libdd_trace_model`] traits, mirroring the
//! agent's `normalizeV1` and `normalizeTraceChunkV1` (`pkg/trace/agent/normalizer.go`). Senders
//! that bypass the agent (agentless) apply it in the agent's place.

use crate::normalize_utils;
use crate::normalizer::is_valid_http_status_code;
use libdd_trace_model::{
    AttributeValue, Attributes as _, AttributesMut as _, SpanMut, TraceChunkMut, TraceText,
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

    if span.has_attribute(HTTP_STATUS_CODE_KEY) {
        let valid = match span.attribute_str(HTTP_STATUS_CODE_KEY) {
            Some(code) => is_valid_http_status_code(code),
            None => span
                .attribute_i64(HTTP_STATUS_CODE_KEY)
                .is_some_and(|code| (100..600).contains(&code)),
        };
        if !valid {
            span.remove_attribute(HTTP_STATUS_CODE_KEY);
        }
    }

    for link in span.span_links_mut() {
        if let Some(v) = link
            .attribute_str(LINK_NAME_KEY)
            .and_then(|v| normalized(v, normalize_utils::normalize_name))
        {
            link.set_attribute(
                TraceText::from_static(LINK_NAME_KEY),
                AttributeValue::String(v.into()),
            );
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
    use libdd_trace_model::{Attributes, AttributesMut, Span, StringData, TraceChunk};
    use std::collections::HashMap;

    type Value = AttributeValue<StringData>;

    #[derive(Default)]
    struct TestLink {
        attrs: HashMap<String, Value>,
    }

    #[derive(Default)]
    struct TestSpan {
        service: String,
        name: String,
        resource: String,
        typ: String,
        span_id: u64,
        parent_id: u64,
        start: i64,
        duration: i64,
        env: Option<String>,
        attrs: HashMap<String, Value>,
        links: Vec<TestLink>,
    }

    #[derive(Default)]
    struct TestChunk {
        trace_id: u128,
        spans: Vec<TestSpan>,
        attrs: HashMap<String, Value>,
    }

    macro_rules! impl_attributes {
        ($($ty:ty),*) => {$(
            impl Attributes for $ty {
                type Text = String;
                type Bytes = Vec<u8>;
                type Values = StringData;
                fn attribute_str(&self, key: &str) -> Option<&str> {
                    self.attrs.get(key).and_then(Value::as_str)
                }
                fn attribute_f64(&self, key: &str) -> Option<f64> {
                    match self.attrs.get(key)? {
                        Value::Int(i) => Some(*i as f64),
                        Value::Float(f) => Some(*f),
                        _ => None,
                    }
                }
                fn has_attribute(&self, key: &str) -> bool {
                    self.attrs.contains_key(key)
                }
                fn attribute(
                    &self,
                    key: &str,
                ) -> Option<libdd_trace_model::AttributeRef<'_, StringData>> {
                    self.attrs.get(key).map(Into::into)
                }
            }
            impl AttributesMut for $ty {
                fn retain_attributes(&mut self, mut f: impl FnMut(&str, &mut Value) -> bool) {
                    self.attrs.retain(|k, v| f(k, v));
                }
                fn attribute_mut(&mut self, key: &str) -> Option<&mut Value> {
                    self.attrs.get_mut(key)
                }
                fn set_attribute(&mut self, key: String, value: Value) {
                    self.attrs.insert(key, value);
                }
                fn remove_attribute(&mut self, key: &str) -> Option<Value> {
                    self.attrs.remove(key)
                }
            }
        )*};
    }
    impl_attributes!(TestLink, TestSpan, TestChunk);

    impl libdd_trace_model::SpanEvent for TestLink {
        fn name(&self) -> &str {
            ""
        }
        fn time_unix_nano(&self) -> u64 {
            0
        }
    }

    impl libdd_trace_model::SpanLink for TestLink {
        fn trace_id(&self) -> u128 {
            0
        }
        fn span_id(&self) -> u64 {
            0
        }
        fn tracestate(&self) -> &str {
            ""
        }
        fn flags(&self) -> u32 {
            0
        }
    }

    impl Span for TestSpan {
        type SpanEvent = TestLink;
        type SpanLink = TestLink;
        fn service(&self) -> &str {
            &self.service
        }
        fn resource(&self) -> &str {
            &self.resource
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn typ(&self) -> &str {
            &self.typ
        }
        fn span_id(&self) -> u64 {
            self.span_id
        }
        fn parent_id(&self) -> u64 {
            self.parent_id
        }
        fn start(&self) -> i64 {
            self.start
        }
        fn duration(&self) -> i64 {
            self.duration
        }
        fn is_error(&self) -> bool {
            false
        }
        fn span_kind(&self) -> Option<&str> {
            None
        }
        fn env(&self) -> Option<&str> {
            self.env.as_deref()
        }
        fn version(&self) -> Option<&str> {
            None
        }
        fn component(&self) -> Option<&str> {
            None
        }
        fn span_events(&self) -> impl Iterator<Item = &TestLink> {
            std::iter::empty()
        }
        fn span_links(&self) -> impl Iterator<Item = &TestLink> {
            self.links.iter()
        }
    }

    impl SpanMut for TestSpan {
        fn set_service(&mut self, value: String) {
            self.service = value;
        }
        fn set_resource(&mut self, value: String) {
            self.resource = value;
        }
        fn set_name(&mut self, value: String) {
            self.name = value;
        }
        fn set_type(&mut self, value: String) {
            self.typ = value;
        }
        fn set_parent_id(&mut self, value: u64) {
            self.parent_id = value;
        }
        fn set_start(&mut self, value: i64) {
            self.start = value;
        }
        fn set_duration(&mut self, value: i64) {
            self.duration = value;
        }
        fn set_error(&mut self, _: bool) {}
        fn set_span_kind(&mut self, _: &str) {}
        fn set_env(&mut self, value: String) {
            self.env = Some(value);
        }
        fn set_version(&mut self, _: String) {}
        fn set_component(&mut self, _: String) {}
        fn span_events_mut(&mut self) -> impl Iterator<Item = &mut TestLink> {
            std::iter::empty()
        }
        fn span_links_mut(&mut self) -> impl Iterator<Item = &mut TestLink> {
            self.links.iter_mut()
        }
    }

    impl TraceChunk for TestChunk {
        type Span = TestSpan;
        fn trace_id(&self) -> u128 {
            self.trace_id
        }
        fn origin(&self) -> &str {
            ""
        }
        fn spans(&self) -> impl Iterator<Item = &TestSpan> {
            self.spans.iter()
        }
        fn priority(&self) -> Option<i32> {
            None
        }
        fn sampling_mechanism(&self) -> Option<u32> {
            None
        }
        fn dropped_trace(&self) -> bool {
            false
        }
    }

    impl TraceChunkMut for TestChunk {
        fn set_origin(&mut self, _: String) {}
        fn set_priority(&mut self, _: i32) {}
        fn set_sampling_mechanism(&mut self, _: u32) {}
        fn set_dropped_trace(&mut self, _: bool) {}
        fn spans_mut(&mut self) -> impl Iterator<Item = &mut TestSpan> {
            self.spans.iter_mut()
        }
        fn retain_spans(&mut self, f: impl FnMut(&mut TestSpan) -> bool) {
            self.spans.retain_mut(f);
        }
    }

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
        let mut span = valid_span();
        span.links.push(TestLink {
            attrs: HashMap::from([(LINK_NAME_KEY.to_owned(), Value::String(String::new()))]),
        });
        span.links.push(TestLink::default());
        let span = normalize(span);
        assert_eq!(
            span.links[0].attribute_str(LINK_NAME_KEY),
            Some("unnamed_operation")
        );
        assert!(!span.links[1].has_attribute(LINK_NAME_KEY));
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
