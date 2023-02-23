// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use datadog_trace_obfuscation::replacer;
use datadog_trace_protobuf::pb;

fn criterion_benchmark(c: &mut Criterion) {
    let rules: &[replacer::ReplaceRule] = &replacer::parse_rules_from_string(&[
        ["http.url", "(token/)([^/]*)", "${1}?"],
        ["http.url", "guid", "[REDACTED]"],
        ["*", "(token/)([^/]*)", "${1}?"],
        ["*", "this", "that"],
        ["custom.tag", "(/foo/bar/).*", "${1}extra"],
        ["resource.name", "prod", "stage"],
    ])
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
    };

    let mut trace = [span_1];
    c.bench_function("replace_trace_tags_bench", |b| {
        b.iter(|| {
            replacer::replace_trace_tags(black_box(&mut trace), black_box(rules));
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
