// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Span and chunk types implementing the `libdd-trace-model` traits, backed by `HashMap`s, for the
//! unit tests and the benchmarks. The benchmarks include this file with `#[path]`, so it only
//! refers to other crates.
#![allow(dead_code)]

use libdd_trace_model::{
    AttributeRef, AttributeValue, Attributes, AttributesMut, Span, SpanMut, StringData, TraceChunk,
    TraceChunkMut,
};
use std::cell::Cell;
use std::collections::HashMap;

pub type Value = AttributeValue<StringData>;

#[derive(Clone, Default)]
pub struct TestLink {
    pub attrs: HashMap<String, Value>,
}

#[derive(Clone, Default)]
pub struct TestSpan {
    pub service: String,
    pub name: String,
    pub resource: String,
    pub typ: String,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub env: Option<String>,
    pub attrs: HashMap<String, Value>,
    pub links: Vec<TestLink>,
}

#[derive(Clone, Default)]
pub struct TestChunk {
    pub trace_id: u128,
    pub spans: Vec<TestSpan>,
    pub attrs: HashMap<String, Value>,
}

thread_local! {
    static ATTRIBUTE_MUT_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// The number of `AttributesMut::attribute_mut` calls on the test types, on the current thread.
pub fn attribute_mut_calls() -> usize {
    ATTRIBUTE_MUT_CALLS.with(Cell::get)
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
            fn attribute(&self, key: &str) -> Option<AttributeRef<'_, StringData>> {
                self.attrs.get(key).map(Into::into)
            }
        }
        impl AttributesMut for $ty {
            fn retain_attributes(&mut self, mut f: impl FnMut(&str, &mut Value) -> bool) {
                self.attrs.retain(|k, v| f(k, v));
            }
            fn attribute_mut(&mut self, key: &str) -> Option<&mut Value> {
                ATTRIBUTE_MUT_CALLS.with(|n| n.set(n.get() + 1));
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

/// A span that is already normalized, with the fields and attributes that `normalizer_v1` reads.
pub fn normalized_span() -> TestSpan {
    let string = |s: &str| Value::String(s.to_owned());
    TestSpan {
        service: "django".to_owned(),
        name: "django.controller".to_owned(),
        resource: "GET /some/raclette".to_owned(),
        typ: "http".to_owned(),
        span_id: 1388,
        parent_id: 1111,
        start: 1_448_466_874_000_000_000,
        duration: 10_000_000,
        env: Some("prod".to_owned()),
        attrs: HashMap::from([
            ("peer.service".to_owned(), string("postgres")),
            ("_dd.base_service".to_owned(), string("django-app")),
            ("http.status_code".to_owned(), string("200")),
            ("user".to_owned(), string("leo")),
            ("cheese_weight".to_owned(), Value::Float(100000.0)),
        ]),
        links: vec![TestLink {
            attrs: HashMap::from([("link.name".to_owned(), string("next_request"))]),
        }],
    }
}
