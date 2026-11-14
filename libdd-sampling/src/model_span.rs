// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sampling support for spans implementing the [`libdd_trace_model`] traits.
//!
//! [`DatadogSampler::sample_chunk_root`] makes a decision for the root span of a trace chunk, and
//! [`DdSamplingResult::apply_to_chunk`] writes it back to the chunk.
//!
//! Internally the span is adapted to the crate's sampling traits (see [`crate::types`]).

use std::borrow::Cow;
use std::marker::PhantomData;

use libdd_trace_model::{
    AttributeRef, AttributeValue, AttributesIter, ChunkSpanView, Span, TraceChunk, TraceChunkMut,
    TraceText, ValueTypes,
};

use crate::DatadogSampler;
use crate::datadog_sampler::DdSamplingResult;
use crate::dd_constants::{SAMPLING_DECISION_MAKER_TAG_KEY, SAMPLING_PRIORITY_TAG_KEY};
use crate::types::{AttributeFactory, AttributeLike, SamplingData, SpanProperties, ValueLike};

const HTTP_STATUS_CODE: &str = "http.status_code";
const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";

/// Maps OpenTelemetry semantic-convention attribute keys to the Datadog `http.*` keys sampling
/// rules are written against.
fn otel_to_dd_http_key(key: &str) -> Option<&'static str> {
    match key {
        "http.request.method" => Some("http.method"),
        "http.response.status_code" => Some(HTTP_STATUS_CODE),
        "url.full" => Some("http.url"),
        "user_agent.original" => Some("http.useragent"),
        _ => None,
    }
}

/// Converts a numeric status code attribute, rejecting values outside `1..=u32::MAX`.
fn f64_to_status_code(value: f64) -> Option<u32> {
    // The range check makes the truncating cast lossless apart from dropping a fractional part,
    // matching how v0.4 metrics are read.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (value.is_finite() && value >= 1.0 && value <= f64::from(u32::MAX)).then_some(value as u32)
}

/// The HTTP status code under either naming convention, numeric values first.
fn status_code(span: &impl Span) -> Option<u32> {
    [HTTP_STATUS_CODE, HTTP_RESPONSE_STATUS_CODE]
        .into_iter()
        .find_map(|key| {
            span.attribute_f64(key)
                .and_then(f64_to_status_code)
                .or_else(|| span.attribute_str(key)?.parse().ok())
        })
}

impl<T: ValueTypes> ValueLike for AttributeRef<'_, T> {
    fn as_float(&self) -> Option<f64> {
        self.as_f64()
    }

    fn as_str(&self) -> Option<Cow<'_, str>> {
        match *self {
            Self::String(s) => Some(Cow::Borrowed(s)),
            Self::Int(i) => Some(Cow::Owned(i.to_string())),
            Self::Float(f) => Some(Cow::Owned(f.to_string())),
            Self::Bool(b) => Some(Cow::Borrowed(if b { "true" } else { "false" })),
            Self::Bytes(_) | Self::Array(_) | Self::KeyValueList(_) => None,
        }
    }
}

/// An attribute borrowed from a model span.
pub(crate) struct ModelAttribute<'a, T: ValueTypes> {
    key: &'a str,
    value: AttributeRef<'a, T>,
}

impl<'a, T: ValueTypes> AttributeLike for ModelAttribute<'a, T> {
    type Value = AttributeRef<'a, T>;

    fn key(&self) -> &str {
        self.key
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

/// Span properties borrowed from a model span.
pub(crate) struct ModelSpanProperties<'a, S> {
    span: &'a S,
    status_code: Option<u32>,
}

impl<'a, S: Span> ModelSpanProperties<'a, S> {
    pub(crate) fn new(span: &'a S) -> Self {
        Self {
            span,
            status_code: status_code(span),
        }
    }
}

impl<S: Span + AttributesIter> SpanProperties for ModelSpanProperties<'_, S> {
    type Attribute<'b>
        = ModelAttribute<'b, S::Values>
    where
        Self: 'b;

    fn operation_name(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.name())
    }

    fn service(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.service())
    }

    fn env(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.env().unwrap_or_default())
    }

    fn resource(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.resource())
    }

    fn status_code(&self) -> Option<u32> {
        self.status_code
    }

    /// Yields the attributes through [`Span::compat`], promoted span tags included, so tag rules
    /// see them as v0.4 `meta` would.
    fn attributes(&self) -> impl Iterator<Item = Self::Attribute<'_>> + '_ {
        self.span
            .compat()
            .attributes()
            .map(|(key, value)| ModelAttribute { key, value })
    }

    fn get_alternate_key<'b>(&self, key: &'b str) -> Option<Cow<'b, str>> {
        otel_to_dd_http_key(key).map(Cow::Borrowed)
    }
}

/// A model root span with its trace id and parent sampling context.
pub(crate) struct ModelSamplingData<'a, S> {
    span: &'a S,
    trace_id: u128,
    is_parent_sampled: Option<bool>,
}

impl<S: Span + AttributesIter> SamplingData for ModelSamplingData<'_, S> {
    type TraceId = u128;

    type Properties<'b>
        = ModelSpanProperties<'b, S>
    where
        Self: 'b;

    fn is_parent_sampled(&self) -> Option<bool> {
        self.is_parent_sampled
    }

    fn trace_id(&self) -> &u128 {
        &self.trace_id
    }

    fn with_span_properties<St, R, F>(&self, s: &St, f: F) -> R
    where
        F: for<'b> Fn(&St, &ModelSpanProperties<'b, S>) -> R,
    {
        f(s, &ModelSpanProperties::new(self.span))
    }
}

impl DatadogSampler {
    /// Samples the root span of a trace chunk.
    ///
    /// If `is_parent_sampled` is set the parent decision is inherited. Otherwise the order of
    /// precedence is: a matching sampling rule (rate limited on keep), then the Agent-provided
    /// per-service rate, then keep.
    ///
    /// Rules match against the span's attributes, falling back to the chunk attributes for keys
    /// the span doesn't have. Tag rules also see the promoted span tags (`env`, `version`,
    /// `component`, `span.kind`) under their v0.4 names, but not the promoted chunk tags such as
    /// `_dd.origin`.
    pub fn sample_chunk_root<C>(
        &self,
        root: &ChunkSpanView<'_, C>,
        is_parent_sampled: Option<bool>,
    ) -> DdSamplingResult
    where
        C: TraceChunk + AttributesIter,
        C::Span: AttributesIter,
    {
        self.sample(&ModelSamplingData {
            span: root,
            trace_id: root.chunk.trace_id(),
            is_parent_sampled,
        })
    }
}

/// Builds sampling tags as model attribute values.
struct ChunkTagFactory<T>(PhantomData<T>);

impl<T: ValueTypes> AttributeFactory for ChunkTagFactory<T> {
    type Attribute = (&'static str, AttributeValue<T>);

    fn create_i64(&self, key: &'static str, value: i64) -> Self::Attribute {
        (key, AttributeValue::Int(value))
    }

    fn create_f64(&self, key: &'static str, value: f64) -> Self::Attribute {
        (key, AttributeValue::Float(value))
    }

    fn create_string(&self, key: &'static str, value: Cow<'static, str>) -> Self::Attribute {
        let value = match value {
            Cow::Borrowed(s) => T::Text::from_static(s),
            Cow::Owned(s) => T::Text::from(s),
        };
        (key, AttributeValue::String(value))
    }
}

impl DdSamplingResult {
    /// Writes this decision back to a trace chunk.
    ///
    /// The priority and sampling mechanism go to the chunk's dedicated fields (encoders derive
    /// `_sampling_priority_v1` and `_dd.p.dm` from them); the rate tags go to the chunk
    /// attributes. Does nothing for decisions inherited from the parent, which carry no tags;
    /// use [`Self::get_priority`] for those.
    pub fn apply_to_chunk<C: TraceChunkMut>(&self, chunk: &mut C) {
        let Some(info) = self.get_trace_root_sampling_info() else {
            return;
        };
        let Some(tags) = self.to_dd_sampling_tags(&ChunkTagFactory::<C::Values>(PhantomData))
        else {
            return;
        };
        for (key, value) in tags {
            if key != SAMPLING_PRIORITY_TAG_KEY && key != SAMPLING_DECISION_MAKER_TAG_KEY {
                chunk.set_attribute(C::Text::from_static(key), value);
            }
        }
        chunk.set_priority(i32::from(self.get_priority().into_i8()));
        chunk.set_sampling_mechanism(u32::from(info.mechanism().into_u8()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use libdd_trace_model::{AttributeValue, ChunkSpanView};
    use libdd_trace_utils::span::SliceData;
    use libdd_trace_utils::span::v1::{SpanSlice, TraceChunkSlice};

    use super::*;
    use crate::{SamplingRule, mechanism, priority};

    type Value = AttributeValue<SliceData<'static>>;

    fn str_value(s: &'static str) -> Value {
        AttributeValue::String(Cow::Borrowed(s))
    }

    fn chunk(span: SpanSlice<'static>) -> TraceChunkSlice<'static> {
        TraceChunkSlice {
            trace_id: 42u128.to_be_bytes(),
            spans: vec![span],
            ..Default::default()
        }
    }

    fn root_span() -> SpanSlice<'static> {
        SpanSlice {
            service: Cow::Borrowed("svc"),
            name: Cow::Borrowed("op"),
            resource: Cow::Borrowed("res"),
            ..Default::default()
        }
    }

    fn tag_rule(rate: f64, key: &str, value: &str) -> SamplingRule {
        let tags = HashMap::from([(key.to_string(), value.to_string())]);
        SamplingRule::new(rate, None, None, None, Some(tags), None)
    }

    fn sample(sampler: &DatadogSampler, chunk: &TraceChunkSlice<'static>) -> DdSamplingResult {
        let view = ChunkSpanView {
            span: &chunk.spans[0],
            chunk,
        };
        sampler.sample_chunk_root(&view, None)
    }

    #[test]
    fn rule_match_is_applied_to_chunk_fields_and_attributes() {
        let sampler = DatadogSampler::new(
            vec![SamplingRule::new(
                1.0,
                Some("svc".to_string()),
                None,
                None,
                None,
                None,
            )],
            100,
        );
        let mut chunk = chunk(root_span());
        let result = sample(&sampler, &chunk);
        assert_eq!(result.get_priority(), priority::USER_KEEP);

        result.apply_to_chunk(&mut chunk);
        assert_eq!(chunk.priority, Some(2));
        assert_eq!(
            chunk.sampling_mechanism,
            Some(u32::from(
                mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.into_u8()
            ))
        );
        assert_eq!(
            chunk
                .attributes
                .get("_dd.rule_psr")
                .and_then(AttributeValue::as_f64),
            Some(1.0)
        );
        assert_eq!(
            chunk
                .attributes
                .get("_dd.p.ksr")
                .and_then(AttributeValue::as_str),
            Some("1")
        );
        // Priority and decision maker live in dedicated fields, not attributes.
        assert!(!chunk.attributes.contains_key("_sampling_priority_v1"));
        assert!(!chunk.attributes.contains_key("_dd.p.dm"));
    }

    #[test]
    fn inherited_decision_is_not_applied() {
        let sampler = DatadogSampler::new(vec![], 100);
        let mut chunk = chunk(root_span());
        let view = ChunkSpanView {
            span: &chunk.spans[0],
            chunk: &chunk,
        };
        let result = sampler.sample_chunk_root(&view, Some(false));
        assert_eq!(result.get_priority(), priority::AUTO_REJECT);

        result.apply_to_chunk(&mut chunk);
        assert_eq!(chunk.priority, None);
        assert_eq!(chunk.sampling_mechanism, None);
        assert!(chunk.attributes.is_empty());
    }

    #[test]
    fn tag_rule_matches_dedicated_env_field() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "env", "prod")], 100);
        let mut span = root_span();
        span.env = Cow::Borrowed("prod");
        let result = sample(&sampler, &chunk(span));
        assert_eq!(result.get_priority(), priority::USER_REJECT);
    }

    #[test]
    fn tag_rule_matches_chunk_attribute() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "team", "core")], 100);
        let mut chunk = chunk(root_span());
        chunk
            .attributes
            .insert(Cow::Borrowed("team"), str_value("core"));
        let result = sample(&sampler, &chunk);
        assert_eq!(result.get_priority(), priority::USER_REJECT);
    }

    #[test]
    fn http_rule_matches_otel_attribute_key() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "http.method", "GET")], 100);
        let mut span = root_span();
        span.attributes
            .insert(Cow::Borrowed("http.request.method"), str_value("GET"));
        let result = sample(&sampler, &chunk(span));
        assert_eq!(result.get_priority(), priority::USER_REJECT);
    }

    #[test]
    fn status_code_rule_matches_int_attribute() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "http.status_code", "5*")], 100);
        let mut span = root_span();
        span.attributes.insert(
            Cow::Borrowed("http.response.status_code"),
            AttributeValue::Int(503),
        );
        let result = sample(&sampler, &chunk(span));
        assert_eq!(result.get_priority(), priority::USER_REJECT);
    }

    #[test]
    fn tag_rule_ignores_shadowed_duplicate_attribute() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "team", "core")], 100);
        let mut span = root_span();
        // `VecMap::insert` appends, so the stale "core" value stays in storage, shadowed.
        span.attributes
            .insert(Cow::Borrowed("team"), str_value("core"));
        span.attributes
            .insert(Cow::Borrowed("team"), str_value("edge"));
        let result = sample(&sampler, &chunk(span));
        assert!(result.get_priority().is_keep());
    }

    #[test]
    fn tag_rule_prefers_span_field_over_chunk_attribute() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "env", "prod")], 100);
        let mut span = root_span();
        span.env = Cow::Borrowed("staging");
        let mut chunk = chunk(span);
        chunk
            .attributes
            .insert(Cow::Borrowed("env"), str_value("prod"));
        // The chunk-level default doesn't apply: the span has its own `env`.
        let result = sample(&sampler, &chunk);
        assert!(result.get_priority().is_keep());

        // Without its own value, the span inherits the chunk default.
        chunk.spans[0].env = Cow::Borrowed("");
        let result = sample(&sampler, &chunk);
        assert_eq!(result.get_priority(), priority::USER_REJECT);
    }

    #[test]
    fn unmatched_rule_keeps_by_default() {
        let sampler = DatadogSampler::new(vec![tag_rule(0.0, "env", "prod")], 100);
        let result = sample(&sampler, &chunk(root_span()));
        assert!(result.get_priority().is_keep());
    }
}
