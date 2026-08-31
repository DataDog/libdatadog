// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use criterion::{black_box, criterion_group, Criterion};
use libdd_tinybytes::BytesString;
use libdd_trace_obfuscation::replacer;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::v04::{self, VecMap};

#[allow(clippy::unwrap_used)]
fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("tags");
    let rules: &[replacer::ReplaceRule] = &replacer::parse_rules_from_string(
        r#"[
        {"name": "*", "pattern": "(token/)([^/]*)", "repl": "${1}?"},
        {"name": "*", "pattern": "this", "repl": "that"},
        {"name": "http.url", "pattern": "guid", "repl": "[REDACTED]"},
        {"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"},
        {"name": "resource.name", "pattern": "prod", "repl": "stage"}
    ]"#,
    )
    .unwrap();

    let span_1 = pb::Span {
        duration: 10000000,
        error: 0,
        resource: "GET /some/raclette".to_string(),
        service: "django".to_string(),
        name: "django.controller".to_string(),
        span_id: 123,
        start: 1448466874000000000,
        trace_id: 424242,
        meta: HashMap::from([
            ("resource.name".to_string(), "this is prod".to_string()),
            (
                "http.url".to_string(),
                "some/[REDACTED]/token/abcdef/abc".to_string(),
            ),
            (
                "other.url".to_string(),
                "some/guid/token/abcdef/abc".to_string(),
            ),
            ("custom.tag".to_string(), "/foo/bar/foo".to_string()),
        ]),
        metrics: HashMap::from([("cheese_weight".to_string(), 100000.0)]),
        parent_id: 1111,
        r#type: "http".to_string(),
        meta_struct: HashMap::new(),
        span_links: vec![],
        span_events: vec![],
    };

    let trace = [span_1];
    group.bench_function("replace_trace_tags", |b| {
        b.iter_batched_ref(
            || trace.to_owned(),
            |t| replacer::replace_trace_tags(black_box(t), black_box(rules)),
            criterion::BatchSize::LargeInput,
        );
    });

    // v04 span equivalent of `span_1`, using the immutable `BytesString` text type.
    let bs = |s: &str| BytesString::from_slice(s.as_ref()).unwrap();
    let span_1_v04 = v04::SpanBytes {
        duration: 10000000,
        error: 0,
        resource: bs("GET /some/raclette"),
        service: bs("django"),
        name: bs("django.controller"),
        span_id: 123,
        start: 1448466874000000000,
        trace_id: 424242,
        meta: vec![
            (bs("resource.name"), bs("this is prod")),
            (bs("http.url"), bs("some/[REDACTED]/token/abcdef/abc")),
            (bs("other.url"), bs("some/guid/token/abcdef/abc")),
            (bs("custom.tag"), bs("/foo/bar/foo")),
        ]
        .into(),
        metrics: vec![(bs("cheese_weight"), 100000.0)].into(),
        parent_id: 1111,
        r#type: bs("http"),
        meta_struct: VecMap::default(),
        span_links: vec![],
        span_events: vec![],
    };

    let trace_v04 = [span_1_v04];
    group.bench_function("replace_trace_tags_v04", |b| {
        b.iter_batched_ref(
            || trace_v04.to_owned(),
            |t| {
                for span in t.iter_mut() {
                    replacer::replace_span_tags_v04(black_box(span), black_box(rules));
                }
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, criterion_benchmark);
