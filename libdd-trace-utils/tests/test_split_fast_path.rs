// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_trace_model::{AttributeValue, TraceText, ValueTypes};
use libdd_trace_utils::{
    msgpack_encoder::{
        split::{self, Format, Outcome},
        v1, v04,
    },
    span::{
        SpanText, TraceData,
        v1::{Span, TraceChunk, TracerPayload},
        vec_map::VecMap,
    },
};
use serde::Serialize;
use std::{borrow::Borrow, cell::Cell};

thread_local! {
    static READS: Cell<usize> = const { Cell::new(0) };
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize)]
struct Text(String);

impl Borrow<str> for Text {
    fn borrow(&self) -> &str {
        READS.with(|count| count.set(count.get() + 1));
        &self.0
    }
}

impl From<String> for Text {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl TraceText for Text {
    fn from_static(value: &'static str) -> Self {
        Self(value.into())
    }
}

impl SpanText for Text {
    fn from_static_str(value: &'static str) -> Self {
        Self(value.into())
    }
    fn from_owned(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Data;

impl ValueTypes for Data {
    type Text = Text;
    type Bytes = &'static [u8];
    type Array = Vec<AttributeValue<Self>>;
    type Map = VecMap<Text, AttributeValue<Self>>;
}

impl TraceData for Data {}

fn payload(chunks: usize) -> TracerPayload<Data> {
    TracerPayload {
        env: Text::from_static("production"),
        chunks: (0..chunks)
            .map(|_| TraceChunk {
                spans: vec![Span {
                    resource: Text::from_static("request"),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn reference(source: &TracerPayload<Data>, format: Format) -> Vec<u8> {
    match format {
        Format::V04 => v04::to_vec_from_v1(source),
        Format::V1 => v1::to_vec_from_v1(source),
    }
}

#[test]
fn fitting_payload_is_lazy_and_reads_the_input_only_once() {
    let source = payload(1);
    for format in [Format::V04, Format::V1] {
        READS.set(0);
        let expected = reference(&source, format);
        let reads = READS.get();
        assert!(reads > 0);
        READS.set(0);
        let mut results = split::encode(&source, format, expected.len());
        assert_eq!(READS.get(), 0);
        let Some(Outcome::Encoded { bytes, .. }) = results.next() else {
            panic!("expected payload")
        };
        assert_eq!(bytes, expected);
        assert_eq!(READS.get(), reads);
        assert!(results.next().is_none());
        assert_eq!(READS.get(), reads);
    }
}

#[test]
fn stopping_after_the_first_split_does_not_encode_the_pending_half() {
    let source = payload(2);
    let left = payload(1);
    for format in [Format::V04, Format::V1] {
        READS.set(0);
        let full_size = reference(&source, format).len();
        let expected = reference(&left, format);
        assert!(full_size > expected.len());
        let reads = READS.get();

        READS.set(0);
        let Some(Outcome::Encoded { chunks, bytes }) =
            split::encode(&source, format, expected.len()).next()
        else {
            panic!("expected first half");
        };
        assert_eq!(chunks, 0..1);
        assert_eq!(bytes, expected);
        assert_eq!(READS.get(), reads);
    }
}
