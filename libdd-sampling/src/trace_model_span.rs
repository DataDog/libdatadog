// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sampling trait implementations for the [`libdd_trace_model`] types.
//!
//! Any type implementing the model's `Span` trait can be sampled through
//! [`ModelSamplingData`]; sampling tags can be written back to the span with
//! [`ModelSamplingAttribute::apply_to`].

use std::borrow::Cow;
use std::marker::PhantomData;

use libdd_trace_model::{AttributeValue, Span as ModelSpan, TraceText, ValueTypes};

use crate::types::{
    AttributeFactory, AttributeLike, SamplingData, SpanProperties, TraceIdLike, ValueLike,
};

// OpenTelemetry semantic-convention attribute keys.
const HTTP_REQUEST_METHOD: &str = "http.request.method";

// Datadog attribute keys.
const HTTP_METHOD: &str = "http.method";

// Attribute keys used by both conventions.
const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
const HTTP_STATUS_CODE: &str = "http.status_code";

/// A 16-byte trace ID in the OTLP and W3C layout, read as a big-endian integer.
impl TraceIdLike for [u8; 16] {
    fn to_u128(&self) -> u128 {
        u128::from_be_bytes(*self)
    }
}

impl<T: ValueTypes> ValueLike for AttributeValue<T> {
    fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            // `i64 as f64` rounds above 2^53; acceptable here because rule matching only
            // renders integral values back to strings.
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::String(s) => Some(Cow::Borrowed(s.as_str())),
            Self::Int(i) => Some(Cow::Owned(i.to_string())),
            Self::Float(f) => Some(Cow::Owned(f.to_string())),
            Self::Bool(b) => Some(Cow::Owned(b.to_string())),
            // Bytes, arrays, and key-value lists have no string form.
            _ => None,
        }
    }
}

/// A span attribute borrowing its key and value from a model span.
pub struct ModelSpanAttribute<'a, T: ValueTypes> {
    key: &'a str,
    value: &'a AttributeValue<T>,
}

impl<T: ValueTypes> AttributeLike for ModelSpanAttribute<'_, T> {
    type Value = AttributeValue<T>;

    fn key(&self) -> &str {
        self.key
    }

    fn value(&self) -> &Self::Value {
        self.value
    }
}

fn status_code_from(value: Option<&AttributeValue<impl ValueTypes>>) -> Option<u32> {
    match value? {
        AttributeValue::Int(i) => u32::try_from(*i).ok(),
        AttributeValue::String(s) => s.as_str().parse().ok(),
        _ => None,
    }
}

/// Span properties borrowing from any type implementing the model's [`Span`] trait.
pub struct ModelSpanProperties<'a, S: ModelSpan + ?Sized> {
    span: &'a S,
}

impl<'a, S: ModelSpan + ?Sized> ModelSpanProperties<'a, S> {
    /// Builds span properties by borrowing from `span` for lifetime `'a`.
    pub fn from_span(span: &'a S) -> Self {
        Self { span }
    }
}

impl<S: ModelSpan + ?Sized> SpanProperties for ModelSpanProperties<'_, S> {
    type Attribute<'b>
        = ModelSpanAttribute<'b, S::Values>
    where
        Self: 'b;

    fn operation_name(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.name())
    }

    fn service(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.service())
    }

    fn env(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.env())
    }

    fn resource(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.span.resource())
    }

    fn status_code(&self) -> Option<u32> {
        status_code_from(self.span.attribute(HTTP_RESPONSE_STATUS_CODE))
            .or_else(|| status_code_from(self.span.attribute(HTTP_STATUS_CODE)))
    }

    fn attributes(&self) -> impl Iterator<Item = ModelSpanAttribute<'_, S::Values>> + '_ {
        self.span
            .iter_attributes()
            .map(|(key, value)| ModelSpanAttribute { key, value })
    }

    fn get_alternate_key<'b>(&self, key: &'b str) -> Option<Cow<'b, str>> {
        match key {
            HTTP_RESPONSE_STATUS_CODE => Some(Cow::Borrowed(HTTP_STATUS_CODE)),
            HTTP_REQUEST_METHOD => Some(Cow::Borrowed(HTTP_METHOD)),
            _ => None,
        }
    }
}

/// Wraps a reference to a model [`Span`] with the trace ID and parent sampling
/// context needed for a sampling decision.
///
/// `is_parent_sampled` should be `Some(true)`/`Some(false)` when a parent span exists,
/// or `None` for root spans.
pub struct ModelSamplingData<'a, S: ModelSpan + ?Sized> {
    pub is_parent_sampled: Option<bool>,
    pub trace_id: u128,
    pub span: &'a S,
}

impl<S: ModelSpan + ?Sized> SamplingData for ModelSamplingData<'_, S> {
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

    fn with_span_properties<S2, R, F>(&self, s: &S2, f: F) -> R
    where
        F: for<'b> Fn(&S2, &ModelSpanProperties<'b, S>) -> R,
    {
        let props = ModelSpanProperties { span: self.span };
        f(s, &props)
    }
}

/// A sampling attribute produced by [`ModelAttributeFactory`]. It pairs a
/// sampling tag name with a typed model [`AttributeValue`].
pub struct ModelSamplingAttribute<T: ValueTypes> {
    pub key: &'static str,
    pub value: AttributeValue<T>,
}

impl<T: ValueTypes> ModelSamplingAttribute<T> {
    /// Applies this sampling attribute to a span implementing the model's [`Span`] trait.
    pub fn apply_to<S: ModelSpan<Values = T> + ?Sized>(self, span: &mut S) {
        span.set_attribute(S::Text::from_static(self.key), self.value);
    }
}

/// Attribute factory that produces [`ModelSamplingAttribute`] values for any
/// choice of [`ValueTypes`].
pub struct ModelAttributeFactory<T: ValueTypes>(PhantomData<fn() -> T>);

impl<T: ValueTypes> ModelAttributeFactory<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: ValueTypes> Default for ModelAttributeFactory<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ValueTypes> AttributeFactory for ModelAttributeFactory<T> {
    type Attribute = ModelSamplingAttribute<T>;

    fn create_i64(&self, key: &'static str, value: i64) -> Self::Attribute {
        ModelSamplingAttribute {
            key,
            value: AttributeValue::Int(value),
        }
    }

    fn create_f64(&self, key: &'static str, value: f64) -> Self::Attribute {
        ModelSamplingAttribute {
            key,
            value: AttributeValue::Float(value),
        }
    }

    fn create_string(&self, key: &'static str, value: Cow<'static, str>) -> Self::Attribute {
        let text = match value {
            Cow::Borrowed(s) => T::Text::from_static(s),
            Cow::Owned(s) => T::Text::from(s),
        };
        ModelSamplingAttribute {
            key,
            value: AttributeValue::String(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DatadogSampler, priority};
    use libdd_trace_model::{Attributes, ValueMap};

    #[derive(Default)]
    struct TestValues;

    impl ValueTypes for TestValues {
        type Text = String;
        type Bytes = Vec<u8>;
        type Array = Vec<AttributeValue<TestValues>>;
        type Map = Vec<(String, AttributeValue<TestValues>)>;
    }

    impl ValueMap<TestValues> for Vec<(String, AttributeValue<TestValues>)> {
        fn try_get(&self, key: &str) -> Option<&AttributeValue<TestValues>> {
            self.as_slice()
                .iter()
                .find(|entry| entry.0 == key)
                .map(|entry| &entry.1)
        }

        fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a AttributeValue<TestValues>)>
        where
            TestValues: 'a,
        {
            self.as_slice()
                .iter()
                .map(|entry| (entry.0.as_str(), &entry.1))
        }
    }

    #[derive(Default)]
    struct TestSpan {
        name: String,
        env: String,
        service: String,
        resource: String,
        span_type: String,
        attributes: Vec<(String, AttributeValue<TestValues>)>,
    }

    impl TestSpan {
        fn set(&mut self, key: &str, value: AttributeValue<TestValues>) {
            match self.attributes.iter_mut().find(|entry| entry.0 == key) {
                Some(entry) => entry.1 = value,
                None => self.attributes.push((key.to_string(), value)),
            }
        }
    }

    impl Attributes for TestSpan {
        type Text = String;
        type Bytes = Vec<u8>;
        type Values = TestValues;

        fn attribute(&self, key: &str) -> Option<&AttributeValue<TestValues>> {
            self.attributes
                .as_slice()
                .iter()
                .find(|entry| entry.0 == key)
                .map(|entry| &entry.1)
        }

        fn retain_attributes(
            &mut self,
            mut f: impl FnMut(&String, &mut AttributeValue<TestValues>) -> bool,
        ) {
            self.attributes
                .retain_mut(|entry| f(&entry.0, &mut entry.1));
        }

        fn attribute_mut(&mut self, key: &str) -> Option<&mut AttributeValue<TestValues>> {
            self.attributes
                .iter_mut()
                .find(|entry| entry.0 == key)
                .map(|entry| &mut entry.1)
        }

        fn set_attribute(&mut self, key: impl Into<String>, value: AttributeValue<TestValues>) {
            let key = key.into();
            self.set(&key, value);
        }

        fn iter_attributes(&self) -> impl Iterator<Item = (&str, &AttributeValue<TestValues>)> {
            self.attributes
                .as_slice()
                .iter()
                .map(|entry| (entry.0.as_str(), &entry.1))
        }
    }

    impl ModelSpan for TestSpan {
        fn name(&self) -> &str {
            &self.name
        }

        fn env(&self) -> &str {
            &self.env
        }

        fn service(&self) -> &str {
            &self.service
        }

        fn resource(&self) -> &str {
            &self.resource
        }

        fn r#type(&self) -> &str {
            &self.span_type
        }

        fn set_service(&mut self, value: impl Into<String>) {
            self.service = value.into();
        }

        fn set_resource(&mut self, value: impl Into<String>) {
            self.resource = value.into();
        }
    }

    fn make_span() -> TestSpan {
        TestSpan {
            name: "my-operation".into(),
            service: "my-service".into(),
            resource: "GET /api".into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_trace_id_from_bytes() {
        let id: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xca, 0xfe];
        assert_eq!(id.to_u128(), 0xcafe_u128);
    }

    #[test]
    fn test_attribute_value_as_float() {
        assert_eq!(
            AttributeValue::<TestValues>::Float(1.5).as_float(),
            Some(1.5)
        );
        assert_eq!(AttributeValue::<TestValues>::Int(42).as_float(), Some(42.0));
        assert_eq!(
            AttributeValue::<TestValues>::String("x".into()).as_float(),
            None
        );
        assert_eq!(AttributeValue::<TestValues>::Bool(true).as_float(), None);
        assert_eq!(
            AttributeValue::<TestValues>::Bytes(vec![1]).as_float(),
            None
        );
    }

    #[test]
    fn test_attribute_value_as_str() {
        assert_eq!(
            AttributeValue::<TestValues>::String("hello".into()).as_str(),
            Some(Cow::Borrowed("hello"))
        );
        assert_eq!(
            AttributeValue::<TestValues>::Int(7).as_str(),
            Some(Cow::Owned("7".into()))
        );
        assert_eq!(
            AttributeValue::<TestValues>::Float(1.5).as_str(),
            Some(Cow::Owned("1.5".into()))
        );
        assert_eq!(
            AttributeValue::<TestValues>::Bool(true).as_str(),
            Some(Cow::Owned("true".into()))
        );
        assert_eq!(
            AttributeValue::<TestValues>::Bytes(vec![1, 2]).as_str(),
            None
        );
    }

    #[test]
    fn test_model_span_properties_basic_fields() {
        let span = make_span();
        let props = ModelSpanProperties::from_span(&span);
        assert_eq!(props.operation_name(), "my-operation");
        assert_eq!(props.service(), "my-service");
        assert_eq!(props.resource(), "GET /api");
        assert_eq!(props.env(), "");
        assert_eq!(props.status_code(), None);
        assert_eq!(props.attributes().count(), 0);
    }

    #[test]
    fn test_model_span_properties_env_and_attributes() {
        let mut span = make_span();
        span.env = "staging".into();
        span.set("env", AttributeValue::String("staging".into()));
        span.set("http.request.method", AttributeValue::String("GET".into()));

        let props = ModelSpanProperties::from_span(&span);
        assert_eq!(props.env(), "staging");
        assert_eq!(props.attributes().count(), 2);

        let method = props
            .attributes()
            .find(|a| a.key() == "http.request.method")
            .unwrap();
        assert_eq!(method.value().as_str(), Some(Cow::Borrowed("GET")));
    }

    #[test]
    fn test_model_span_properties_status_code() {
        let mut span = make_span();
        span.set("http.response.status_code", AttributeValue::Int(200));
        assert_eq!(
            ModelSpanProperties::from_span(&span).status_code(),
            Some(200)
        );

        let mut span = make_span();
        span.set("http.status_code", AttributeValue::Int(404));
        assert_eq!(
            ModelSpanProperties::from_span(&span).status_code(),
            Some(404)
        );

        let mut span = make_span();
        span.set("http.status_code", AttributeValue::String("500".into()));
        assert_eq!(
            ModelSpanProperties::from_span(&span).status_code(),
            Some(500)
        );

        let mut span = make_span();
        span.set("http.status_code", AttributeValue::Int(-1));
        assert_eq!(ModelSpanProperties::from_span(&span).status_code(), None);
    }

    #[test]
    fn test_model_span_properties_alternate_keys() {
        let span = make_span();
        let props = ModelSpanProperties::from_span(&span);
        assert_eq!(
            props.get_alternate_key("http.response.status_code"),
            Some(Cow::Borrowed("http.status_code"))
        );
        assert_eq!(
            props.get_alternate_key("http.request.method"),
            Some(Cow::Borrowed("http.method"))
        );
        assert_eq!(props.get_alternate_key("anything"), None);
    }

    #[test]
    fn test_model_sampling_data_fields() {
        let span = make_span();
        let data = ModelSamplingData {
            is_parent_sampled: Some(true),
            trace_id: 0xdeadbeef_cafebabe,
            span: &span,
        };
        assert_eq!(data.is_parent_sampled(), Some(true));
        assert_eq!(*data.trace_id(), 0xdeadbeef_cafebabe_u128);
    }

    #[test]
    fn test_attribute_factory() {
        let factory = ModelAttributeFactory::<TestValues>::new();

        let i = factory.create_i64("_sampling_priority_v1", 2);
        assert_eq!(i.key, "_sampling_priority_v1");
        assert_eq!(i.value, AttributeValue::Int(2));

        let f = factory.create_f64("_dd.rule_psr", 0.5);
        assert_eq!(f.key, "_dd.rule_psr");
        assert_eq!(f.value, AttributeValue::Float(0.5));

        let s = factory.create_string("_dd.p.dm", Cow::Borrowed("-3"));
        assert_eq!(s.key, "_dd.p.dm");
        assert_eq!(s.value, AttributeValue::String("-3".into()));

        let owned = factory.create_string("k", Cow::Owned("v".to_string()));
        assert_eq!(owned.value, AttributeValue::String("v".into()));
    }

    #[test]
    fn test_apply_to_span() {
        let mut span = make_span();
        ModelAttributeFactory::<TestValues>::new()
            .create_i64("_sampling_priority_v1", 1)
            .apply_to(&mut span);
        assert_eq!(
            span.attribute("_sampling_priority_v1"),
            Some(&AttributeValue::Int(1))
        );
    }

    #[test]
    fn test_integration_root_span_default_keep() {
        let sampler = DatadogSampler::new(vec![], 100);
        let span = make_span();
        let data = ModelSamplingData {
            is_parent_sampled: None,
            trace_id: 12345,
            span: &span,
        };
        let result = sampler.sample(&data);
        assert!(result.get_priority().is_keep());
    }

    #[test]
    fn test_integration_parent_sampled_propagates() {
        let sampler = DatadogSampler::new(vec![], 100);
        let span = make_span();

        let data = ModelSamplingData {
            is_parent_sampled: Some(true),
            trace_id: 1,
            span: &span,
        };
        assert_eq!(sampler.sample(&data).get_priority(), priority::AUTO_KEEP);

        let data = ModelSamplingData {
            is_parent_sampled: Some(false),
            trace_id: 1,
            span: &span,
        };
        assert_eq!(sampler.sample(&data).get_priority(), priority::AUTO_REJECT);
    }

    #[test]
    fn test_integration_tags_apply_to_span() {
        let sampler = DatadogSampler::new(vec![], 100);
        let mut span = make_span();
        span.env = "prod".into();

        let data = ModelSamplingData {
            is_parent_sampled: None,
            trace_id: 42,
            span: &span,
        };
        let result = sampler.sample(&data);

        let tags = result
            .to_dd_sampling_tags(&ModelAttributeFactory::<TestValues>::new())
            .expect("tags should be produced for root spans");
        assert!(!tags.is_empty());

        for tag in tags {
            tag.apply_to(&mut span);
        }

        assert_eq!(
            span.attribute("_sampling_priority_v1"),
            Some(&AttributeValue::Int(1))
        );
        assert!(span.attribute("_dd.p.dm").is_some());
    }
}
