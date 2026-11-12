// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runs the same cases against the protobuf normalizer and the generic one (on v1 spans), so the
//! two can't drift apart, then covers what only the generic normalizer does.

use libdd_tinybytes::BytesString;
use libdd_trace_normalization::generic::{self, NormalizeError};
use libdd_trace_normalization::normalizer;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::BytesData;
use libdd_trace_utils::span::v1::{self, AttributeValue};
use std::collections::HashMap;
use std::time::SystemTime;

const TRACE_ID: u64 = 424242;
const SPAN_ID: u64 = 4242;
const START: i64 = 1448466874000000000;
const PRIORITY_NONE: i32 = normalizer::SamplerPriority::None as i32;

/// The fields both normalizers read, as v0.4-style strings.
#[derive(Clone)]
struct Input {
    service: String,
    name: String,
    resource: String,
    typ: String,
    span_id: u64,
    parent_id: u64,
    start: i64,
    duration: i64,
    env: Option<String>,
    http_status_code: Option<String>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            service: "django".to_owned(),
            name: "django.controller".to_owned(),
            resource: "GET /some/raclette".to_owned(),
            typ: "http".to_owned(),
            span_id: SPAN_ID,
            parent_id: 1111,
            start: START,
            duration: 10000000,
            env: None,
            http_status_code: None,
        }
    }
}

/// What a normalized span looks like. `start` is `None` when it was reset to the current time.
#[derive(Debug, PartialEq)]
struct Output {
    service: String,
    name: String,
    resource: String,
    typ: String,
    parent_id: u64,
    start: Option<i64>,
    duration: i64,
    env: Option<String>,
    http_status_code: Option<String>,
}

impl Output {
    fn of(input: &Input) -> Self {
        Self {
            service: input.service.clone(),
            name: input.name.clone(),
            resource: input.resource.clone(),
            typ: input.typ.clone(),
            parent_id: input.parent_id,
            start: Some(input.start),
            duration: input.duration,
            env: input.env.clone(),
            http_status_code: input.http_status_code.clone(),
        }
    }
}

fn now() -> i64 {
    i64::try_from(SystemTime::UNIX_EPOCH.elapsed().unwrap().as_nanos()).unwrap()
}

/// `None` if the normalizer replaced `input_start` with a start derived from the current time.
fn observed_start(start: i64, input_start: i64, duration: i64, before: i64) -> Option<i64> {
    if start == input_start {
        return Some(start);
    }
    assert!(
        start >= before.saturating_sub(duration) && start <= now(),
        "start {start}"
    );
    None
}

fn pb_span(input: &Input) -> pb::Span {
    let mut meta = HashMap::new();
    if let Some(env) = &input.env {
        meta.insert("env".to_owned(), env.clone());
    }
    if let Some(code) = &input.http_status_code {
        meta.insert("http.status_code".to_owned(), code.clone());
    }
    pb::Span {
        service: input.service.clone(),
        name: input.name.clone(),
        resource: input.resource.clone(),
        trace_id: TRACE_ID,
        span_id: input.span_id,
        parent_id: input.parent_id,
        start: input.start,
        duration: input.duration,
        r#type: input.typ.clone(),
        meta,
        ..Default::default()
    }
}

fn v1_span(input: &Input) -> v1::Span<BytesData> {
    let mut span = v1::Span::<BytesData> {
        service: BytesString::from(input.service.clone()),
        name: BytesString::from(input.name.clone()),
        resource: BytesString::from(input.resource.clone()),
        r#type: BytesString::from(input.typ.clone()),
        span_id: input.span_id,
        parent_id: input.parent_id,
        start: input.start,
        duration: input.duration,
        env: BytesString::from(input.env.clone().unwrap_or_default()),
        ..Default::default()
    };
    if let Some(code) = &input.http_status_code {
        span.attributes.insert(
            "http.status_code".into(),
            AttributeValue::String(BytesString::from(code.clone())),
        );
    }
    span
}

fn v1_chunk(spans: Vec<v1::Span<BytesData>>) -> v1::TraceChunk<BytesData> {
    v1::TraceChunk {
        trace_id: u128::from(TRACE_ID).to_be_bytes(),
        spans,
        ..Default::default()
    }
}

fn run_pb(input: &Input) -> Option<Output> {
    let before = now();
    let mut trace = [pb_span(input)];
    normalizer::normalize_trace(&mut trace).ok()?;
    let [span] = trace;
    Some(Output {
        start: observed_start(span.start, input.start, span.duration, before),
        env: span.meta.get("env").cloned(),
        http_status_code: span.meta.get("http.status_code").cloned(),
        service: span.service,
        name: span.name,
        resource: span.resource,
        typ: span.r#type,
        parent_id: span.parent_id,
        duration: span.duration,
    })
}

fn run_v1(input: &Input) -> Option<Output> {
    let before = now();
    let mut chunk = v1_chunk(vec![v1_span(input)]);
    generic::normalize_trace(&mut chunk).ok()?;
    Some(v1_output(input, &chunk.spans[0], before))
}

fn v1_output(input: &Input, span: &v1::Span<BytesData>, before: i64) -> Output {
    // v1 holds no env as an empty env; pb keeps the (normalized) empty tag.
    let env = input.env.as_ref().map(|_| span.env.as_str().to_owned());
    Output {
        start: observed_start(span.start, input.start, span.duration, before),
        env,
        http_status_code: span
            .attributes
            .get("http.status_code")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        service: span.service.as_str().to_owned(),
        name: span.name.as_str().to_owned(),
        resource: span.resource.as_str().to_owned(),
        typ: span.r#type.as_str().to_owned(),
        parent_id: span.parent_id,
        duration: span.duration,
    }
}

type Case = (&'static str, fn(&mut Input), fn(&mut Output));

/// Each case edits the default input, then the expected output (which starts as the input).
/// Output edits of `|_| {}` mean the span is unchanged.
const SPAN_CASES: &[Case] = &[
    ("valid span", |_| {}, |_| {}),
    (
        "empty service",
        |i| i.service = "".into(),
        |o| o.service = "unnamed-service".into(),
    ),
    (
        "invalid service",
        |i| i.service = "retargeting(api-Staging ".into(),
        |o| o.service = "retargeting_api-staging".into(),
    ),
    (
        "empty name",
        |i| i.name = "".into(),
        |o| o.name = "unnamed_operation".into(),
    ),
    (
        "name with no alphanumeric",
        |i| i.name = "/".into(),
        |o| o.name = "unnamed_operation".into(),
    ),
    (
        "invalid name",
        |i| i.name = "trace-api.request".into(),
        |o| o.name = "trace_api.request".into(),
    ),
    (
        "long name",
        |i| {
            i.name = "CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT.CAMEMBERT".into()
        },
        |o| o.name = "CAMEMBERT.".repeat(10),
    ),
    (
        "empty resource",
        |i| i.resource = "".into(),
        |o| o.resource = "django.controller".into(),
    ),
    (
        "empty resource and invalid name",
        |i| {
            i.resource = "".into();
            i.name = "bad-name".into();
        },
        |o| {
            o.resource = "bad_name".into();
            o.name = "bad_name".into();
        },
    ),
    (
        "long type",
        |i| i.typ = "sql".repeat(1000),
        |o| o.typ = "sql".repeat(1000)[..100].to_owned(),
    ),
    (
        "zipkin root span",
        |i| {
            i.span_id = TRACE_ID;
            i.parent_id = TRACE_ID;
        },
        |o| o.parent_id = 0,
    ),
    (
        "negative duration",
        |i| i.duration = -50,
        |o| o.duration = 0,
    ),
    (
        "overflowing duration",
        |i| i.duration = i64::MAX,
        |o| o.duration = 0,
    ),
    ("start too small", |i| i.start = 42, |o| o.start = None),
    ("valid env", |i| i.env = Some("prod".into()), |_| {}),
    (
        "invalid env",
        |i| i.env = Some("DEVELOPMENT".into()),
        |o| o.env = Some("development".into()),
    ),
    (
        "env normalized to empty",
        |i| i.env = Some("!!!".into()),
        |o| o.env = Some(String::new()),
    ),
    (
        "valid status code",
        |i| i.http_status_code = Some("200".into()),
        |_| {},
    ),
    (
        "out of range status code",
        |i| i.http_status_code = Some("600".into()),
        |o| o.http_status_code = None,
    ),
    (
        "non numeric status code",
        |i| i.http_status_code = Some("OK".into()),
        |o| o.http_status_code = None,
    ),
];

#[test]
fn normalizers_agree_on_spans() {
    for (case, edit_input, edit_output) in SPAN_CASES {
        let mut input = Input::default();
        edit_input(&mut input);
        let mut expected = Output::of(&input);
        edit_output(&mut expected);
        assert_eq!(run_pb(&input).as_ref(), Some(&expected), "pb: {case}");
        assert_eq!(run_v1(&input).as_ref(), Some(&expected), "v1: {case}");
    }
}

/// Differential fuzzing: on arbitrary spans, both normalizers must agree wherever they share rules.
#[test]
#[cfg_attr(miri, ignore)]
fn normalizers_agree_on_arbitrary_spans() {
    bolero::check!()
        .with_type::<(
            (String, String, String, String), // service, name, resource, type
            (u64, u64),                       // span id, parent id
            (i64, i64),                       // start, duration
            (Option<String>, Option<String>), // env, http.status_code
        )>()
        .cloned()
        .for_each(
            |(
                (service, name, resource, typ),
                (span_id, parent_id),
                (start, duration),
                (env, http_status_code),
            )| {
                let input = Input {
                    service,
                    name,
                    resource,
                    typ,
                    span_id,
                    parent_id,
                    start,
                    duration,
                    env,
                    http_status_code,
                };
                let (pb, v1) = (run_pb(&input), run_v1(&input));
                match (pb, v1) {
                    // Only v1 resets a parent id equal to the span id when the trace id differs.
                    (Some(mut pb), Some(v1)) if parent_id == span_id && span_id != TRACE_ID => {
                        assert_eq!(v1.parent_id, 0);
                        pb.parent_id = 0;
                        assert_eq!(pb, v1);
                    }
                    (pb, v1) => assert_eq!(pb, v1),
                }
            },
        );
}

#[test]
fn normalizers_reject_zero_span_id() {
    let input = Input {
        span_id: 0,
        ..Default::default()
    };
    assert_eq!(run_pb(&input), None);
    assert_eq!(run_v1(&input), None);
    let mut chunk = v1_chunk(vec![v1_span(&input)]);
    assert_eq!(
        generic::normalize_trace(&mut chunk),
        Err(NormalizeError::SpanIdZero)
    );
}

/// Index of the root in the chunks built from a [`ChunkInput`]: not the first span, so that taking
/// the first span instead of the root shows.
const ROOT_INDEX: usize = 1;
/// The first span of every chunk built from a [`ChunkInput`] has this origin, which must be
/// ignored.
const FIRST_SPAN_ORIGIN: &str = "synthetics";

/// A chunk of three spans, the one at [`ROOT_INDEX`] being the root, with these sampling
/// priorities and root origin.
struct ChunkInput {
    chunk_priority: Option<i32>,
    chunk_origin: &'static str,
    span_priorities: [Option<f64>; 3],
    root_origin: Option<&'static str>,
}

fn run_pb_chunk(input: &ChunkInput) -> (Option<i32>, String) {
    let spans = input
        .span_priorities
        .iter()
        .zip(SPAN_ID..)
        .enumerate()
        .map(|(index, (priority, span_id))| {
            let mut span = pb_span(&Input {
                span_id,
                ..Default::default()
            });
            if let Some(priority) = priority {
                span.metrics
                    .insert("_sampling_priority_v1".to_owned(), *priority);
            }
            let origin = match index {
                0 => Some(FIRST_SPAN_ORIGIN),
                ROOT_INDEX => input.root_origin,
                _ => None,
            };
            if let Some(origin) = origin {
                span.meta.insert("_dd.origin".to_owned(), origin.to_owned());
            }
            span
        })
        .collect();
    let mut chunk = pb::TraceChunk {
        priority: input.chunk_priority.unwrap_or(PRIORITY_NONE),
        origin: input.chunk_origin.to_owned(),
        spans,
        ..Default::default()
    };
    normalizer::normalize_chunk(&mut chunk, ROOT_INDEX).unwrap();
    let priority = (chunk.priority != PRIORITY_NONE).then_some(chunk.priority);
    (priority, chunk.origin)
}

fn run_v1_chunk(input: &ChunkInput) -> (Option<i32>, String) {
    let spans = input
        .span_priorities
        .iter()
        .zip(SPAN_ID..)
        .enumerate()
        .map(|(index, (priority, span_id))| {
            let mut span = v1_span(&Input {
                span_id,
                ..Default::default()
            });
            if let Some(priority) = priority {
                span.attributes.insert(
                    "_sampling_priority_v1".into(),
                    AttributeValue::Float(*priority),
                );
            }
            let origin = match index {
                0 => Some(FIRST_SPAN_ORIGIN),
                ROOT_INDEX => input.root_origin,
                _ => None,
            };
            if let Some(origin) = origin {
                span.attributes
                    .insert("_dd.origin".into(), AttributeValue::String(origin.into()));
            }
            span
        })
        .collect();
    let mut chunk = v1_chunk(spans);
    chunk.priority = input.chunk_priority;
    chunk.origin = input.chunk_origin.into();
    generic::normalize_chunk(&mut chunk, ROOT_INDEX).unwrap();
    (chunk.priority, chunk.origin.as_str().to_owned())
}

#[test]
fn normalizers_agree_on_chunks() {
    let cases = [
        (
            "priority from the root",
            ChunkInput {
                chunk_priority: None,
                chunk_origin: "",
                span_priorities: [Some(1.0), Some(2.0), Some(0.0)],
                root_origin: None,
            },
            (Some(2), ""),
        ),
        (
            "priority from another span",
            ChunkInput {
                chunk_priority: None,
                chunk_origin: "",
                span_priorities: [None, None, Some(2.0)],
                root_origin: None,
            },
            (Some(2), ""),
        ),
        (
            "chunk priority kept",
            ChunkInput {
                chunk_priority: Some(0),
                chunk_origin: "",
                span_priorities: [None, Some(2.0), None],
                root_origin: None,
            },
            (Some(0), ""),
        ),
        (
            "no priority anywhere",
            ChunkInput {
                chunk_priority: None,
                chunk_origin: "",
                span_priorities: [None, None, None],
                root_origin: None,
            },
            (None, ""),
        ),
        (
            "origin from the root",
            ChunkInput {
                chunk_priority: Some(1),
                chunk_origin: "",
                span_priorities: [None, None, None],
                root_origin: Some("rum"),
            },
            (Some(1), "rum"),
        ),
        (
            "chunk origin kept",
            ChunkInput {
                chunk_priority: Some(1),
                chunk_origin: "lambda",
                span_priorities: [None, None, None],
                root_origin: Some("rum"),
            },
            (Some(1), "lambda"),
        ),
    ];
    for (case, input, (priority, origin)) in cases {
        let expected = (priority, origin.to_owned());
        assert_eq!(run_pb_chunk(&input), expected, "pb: {case}");
        assert_eq!(run_v1_chunk(&input), expected, "v1: {case}");
    }
}

// What only the generic normalizer does, following the agent's v1 normalizer.

#[test]
fn generic_rejects_empty_chunk() {
    let mut chunk = v1_chunk(vec![]);
    assert_eq!(
        generic::normalize_trace(&mut chunk),
        Err(NormalizeError::EmptyTrace)
    );
}

#[test]
fn generic_accepts_zero_trace_id() {
    let mut chunk = v1_chunk(vec![v1_span(&Input::default())]);
    chunk.trace_id = [0; 16];
    assert_eq!(generic::normalize_trace(&mut chunk), Ok(()));
}

#[test]
fn generic_resets_parent_id_equal_to_span_id() {
    let mut chunk = v1_chunk(vec![v1_span(&Input {
        parent_id: SPAN_ID,
        ..Default::default()
    })]);
    generic::normalize_trace(&mut chunk).unwrap();
    assert_eq!(chunk.spans[0].parent_id, 0);
}

#[test]
fn generic_validates_non_string_status_codes() {
    let cases = [
        (AttributeValue::Int(200), true),
        (AttributeValue::Int(99), false),
        (AttributeValue::Float(404.0), true),
        (AttributeValue::Float(404.5), false),
        (AttributeValue::Bool(true), false),
    ];
    for (value, valid) in cases {
        let mut span = v1_span(&Input::default());
        span.attributes
            .insert("http.status_code".into(), value.clone());
        let mut chunk = v1_chunk(vec![span]);
        generic::normalize_trace(&mut chunk).unwrap();
        assert_eq!(
            chunk.spans[0].attributes.contains_key("http.status_code"),
            valid,
            "{value:?}"
        );
    }
}

#[test]
fn generic_ignores_inexact_root_priority() {
    // The root's priority wins even when it can't be used, like in the agent (which truncates it
    // to 1, where the generic normalizer leaves the priority unset).
    let mut chunk = v1_chunk(vec![v1_span(&Input::default()), v1_span(&Input::default())]);
    chunk.spans[0]
        .attributes
        .insert("_sampling_priority_v1".into(), AttributeValue::Float(1.5));
    chunk.spans[1]
        .attributes
        .insert("_sampling_priority_v1".into(), AttributeValue::Int(2));
    generic::normalize_chunk(&mut chunk, 0).unwrap();
    assert_eq!(chunk.priority, None);
}

#[test]
fn generic_normalize_chunk_rejects_out_of_bounds_root() {
    let mut chunk = v1_chunk(vec![v1_span(&Input::default())]);
    assert_eq!(
        generic::normalize_chunk(&mut chunk, 1),
        Err(NormalizeError::RootSpanIndexOutOfBounds(1))
    );
}
