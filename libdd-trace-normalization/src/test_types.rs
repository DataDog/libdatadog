// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Span and chunk types implementing the `libdd-trace-model` traits, backed by `HashMap`s, for the
//! unit tests and the benchmarks. The benchmarks include this file with `#[path]`, so it only
//! refers to other crates.
#![allow(dead_code)]

use libdd_trace_model::{
    AttributeRef, AttributeValue, Attributes, AttributesMut, Span, SpanMut, TraceChunk,
    TraceChunkMut, TraceText, ValueTypes,
};
use std::cell::Cell;
use std::collections::HashMap;
use std::marker::PhantomData;

/// The [`ValueTypes`] of the test types, with `X` as text, e.g. `String` or `Arc<str>`.
pub struct TestValues<X>(PhantomData<X>);

impl<X: TraceText> ValueTypes for TestValues<X> {
    type Text = X;
    type Bytes = Vec<u8>;
    type Array = Vec<AttributeValue<Self>>;
    type Map = HashMap<X, AttributeValue<Self>>;
}

pub type Value<X = String> = AttributeValue<TestValues<X>>;

#[derive(Clone, Default)]
pub struct TestLink<X: TraceText = String> {
    pub attrs: HashMap<X, Value<X>>,
}

#[derive(Clone, Default)]
pub struct TestSpan<X: TraceText = String> {
    pub service: X,
    pub name: X,
    pub resource: X,
    pub typ: X,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub env: Option<X>,
    pub component: Option<X>,
    pub attrs: HashMap<X, Value<X>>,
    pub links: Vec<TestLink<X>>,
}

#[derive(Clone, Default)]
pub struct TestChunk<X: TraceText = String> {
    pub trace_id: u128,
    pub spans: Vec<TestSpan<X>>,
    pub attrs: HashMap<X, Value<X>>,
}

thread_local! {
    static ATTRIBUTE_MUT_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// The number of `AttributesMut::attribute_mut` calls on the test types, on the current thread.
pub fn attribute_mut_calls() -> usize {
    ATTRIBUTE_MUT_CALLS.with(Cell::get)
}

macro_rules! impl_attributes {
    ($($ty:ident),*) => {$(
        impl<X: TraceText> Attributes for $ty<X> {
            type Text = X;
            type Bytes = Vec<u8>;
            type Values = TestValues<X>;
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
            fn attribute(&self, key: &str) -> Option<AttributeRef<'_, TestValues<X>>> {
                self.attrs.get(key).map(Into::into)
            }
        }
        impl<X: TraceText> AttributesMut for $ty<X> {
            fn retain_attributes(&mut self, mut f: impl FnMut(&str, &mut Value<X>) -> bool) {
                self.attrs.retain(|k, v| f(k.as_str(), v));
            }
            fn attribute_mut(&mut self, key: &str) -> Option<&mut Value<X>> {
                ATTRIBUTE_MUT_CALLS.with(|n| n.set(n.get() + 1));
                self.attrs.get_mut(key)
            }
            fn set_attribute(&mut self, key: X, value: Value<X>) {
                self.attrs.insert(key, value);
            }
            fn remove_attribute(&mut self, key: &str) -> Option<Value<X>> {
                self.attrs.remove(key)
            }
        }
    )*};
}
impl_attributes!(TestLink, TestSpan, TestChunk);

impl<X: TraceText> libdd_trace_model::SpanEvent for TestLink<X> {
    fn name(&self) -> &str {
        ""
    }
    fn time_unix_nano(&self) -> u64 {
        0
    }
}

impl<X: TraceText> libdd_trace_model::SpanLink for TestLink<X> {
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

impl<X: TraceText> Span for TestSpan<X> {
    type SpanEvent = TestLink<X>;
    type SpanLink = TestLink<X>;
    fn service(&self) -> &str {
        self.service.as_str()
    }
    fn resource(&self) -> &str {
        self.resource.as_str()
    }
    fn name(&self) -> &str {
        self.name.as_str()
    }
    fn typ(&self) -> &str {
        self.typ.as_str()
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
        self.env.as_ref().map(X::as_str)
    }
    fn version(&self) -> Option<&str> {
        None
    }
    fn component(&self) -> Option<&str> {
        self.component.as_ref().map(X::as_str)
    }
    fn name_text(&self) -> Option<&X> {
        Some(&self.name)
    }
    fn component_text(&self) -> Option<&X> {
        self.component.as_ref()
    }
    fn span_events(&self) -> impl Iterator<Item = &TestLink<X>> {
        std::iter::empty()
    }
    fn span_links(&self) -> impl Iterator<Item = &TestLink<X>> {
        self.links.iter()
    }
}

impl<X: TraceText> SpanMut for TestSpan<X> {
    fn set_service(&mut self, value: X) {
        self.service = value;
    }
    fn set_resource(&mut self, value: X) {
        self.resource = value;
    }
    fn set_name(&mut self, value: X) {
        self.name = value;
    }
    fn set_type(&mut self, value: X) {
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
    fn set_env(&mut self, value: X) {
        self.env = Some(value);
    }
    fn set_version(&mut self, _: X) {}
    fn set_component(&mut self, value: X) {
        self.component = (!value.as_str().is_empty()).then_some(value);
    }
    fn span_events_mut(&mut self) -> impl Iterator<Item = &mut TestLink<X>> {
        std::iter::empty()
    }
    fn span_links_mut(&mut self) -> impl Iterator<Item = &mut TestLink<X>> {
        self.links.iter_mut()
    }
}

impl<X: TraceText> TraceChunk for TestChunk<X> {
    type Span = TestSpan<X>;
    fn trace_id(&self) -> u128 {
        self.trace_id
    }
    fn origin(&self) -> &str {
        ""
    }
    fn spans(&self) -> impl Iterator<Item = &TestSpan<X>> {
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

impl<X: TraceText> TraceChunkMut for TestChunk<X> {
    fn set_origin(&mut self, _: X) {}
    fn set_priority(&mut self, _: i32) {}
    fn set_sampling_mechanism(&mut self, _: u32) {}
    fn set_dropped_trace(&mut self, _: bool) {}
    fn spans_mut(&mut self) -> impl Iterator<Item = &mut TestSpan<X>> {
        self.spans.iter_mut()
    }
    fn retain_spans(&mut self, f: impl FnMut(&mut TestSpan<X>) -> bool) {
        self.spans.retain_mut(f);
    }
}

/// A span that is already normalized, with the fields and attributes that `normalizer_v1` reads.
pub fn normalized_span<X: TraceText>() -> TestSpan<X> {
    let text = |s: &str| X::from(s.to_owned());
    let string = |s: &str| Value::String(text(s));
    TestSpan {
        service: text("django"),
        name: text("django.controller"),
        resource: text("GET /some/raclette"),
        typ: text("http"),
        span_id: 1388,
        parent_id: 1111,
        start: 1_448_466_874_000_000_000,
        duration: 10_000_000,
        env: Some(text("prod")),
        component: Some(text("django")),
        attrs: HashMap::from([
            (text("peer.service"), string("postgres")),
            (text("_dd.base_service"), string("django-app")),
            (text("http.status_code"), string("200")),
            (text("user"), string("leo")),
            (text("cheese_weight"), Value::Float(100000.0)),
        ]),
        links: vec![TestLink {
            attrs: HashMap::from([(text("link.name"), string("next_request"))]),
        }],
    }
}
