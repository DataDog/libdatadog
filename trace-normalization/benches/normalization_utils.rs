// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use datadog_trace_protobuf::pb;
use std::collections::HashMap;

fn normalize_service_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization/normalize_service");
    let cases = &[
            "",
            "test_ASCII",
            "Test Conversion 0f Weird !@#$%^&**() Characters",
            "Data🐨dog🐶 繋がっ⛰てて",
            "A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000",
        ];

    for case in cases {
        group.bench_with_input(
            BenchmarkId::new(
                "normalize_service",
                if case.is_empty() {
                    "[empty string]"
                } else {
                    case
                },
            ),
            *case,
            |b, case| {
                b.iter_batched_ref(
                    || case.to_owned(),
                    datadog_trace_normalization::normalize_utils::normalize_service,
                    BatchSize::NumBatches(100000),
                )
            },
        );
    }
    group.finish()
}

fn normalize_name_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization/normalize_name");
    let cases = &[
        "good",
        "bad-name",
        "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
    ];
    for case in cases {
        group.bench_with_input(
            BenchmarkId::new("normalize_name", case),
            *case,
            |b, case| {
                b.iter_batched_ref(
                    || case.to_owned(),
                    datadog_trace_normalization::normalize_utils::normalize_name,
                    BatchSize::NumIterations(100000),
                )
            },
        );
    }
    group.finish()
}

fn normalize_span_bench(c: &mut Criterion) {
    let trace = [
        pb::Span {
            duration: 10000000,
            error: 0,
            resource: "GET /some/raclette".to_string(),
            service: "django".to_string(),
            name: "django.controller".to_string(),
            span_id: 1388,
            start: 1448466874000000000,
            trace_id: 424242,
            meta: HashMap::from([
                ("user".to_string(), "leo".to_string()),
                ("pool".to_string(), "fondue".to_string()),
            ]),
            metrics: HashMap::from([("cheese_weight".to_string(), 100000.0)]),
            parent_id: 1111,
            r#type: "http".to_string(),
            meta_struct: HashMap::new(),
            span_links: vec![],
        },
        pb::Span {
            duration: 12000000,
            error: 1,
            resource: "GET /some/reblochon".to_string(),
            service: "".to_string(),
            name: "django.controller".to_string(),
            span_id: 1456,
            start: 1448466849000000000,
            trace_id: 424242,
            meta: HashMap::from([
                ("user".to_string(), "leo".to_string()),
                ("pool".to_string(), "tartiflette".to_string()),
            ]),
            metrics: HashMap::from([("cheese_weight".to_string(), 100000.0)]),
            parent_id: 1123,
            r#type: "http".to_string(),
            meta_struct: HashMap::new(),
            span_links: vec![],
        },
    ];

    c.bench_with_input(
        BenchmarkId::new("normalization/normalize_trace", "test_trace"),
        &trace,
        |b, case| {
            b.iter_batched_ref(
                || case.to_owned(),
                |s| datadog_trace_normalization::normalizer::normalize_trace(s),
                BatchSize::SmallInput,
            )
        },
    );
}

criterion_group!(
    benches,
    normalize_service_bench,
    normalize_name_bench,
    normalize_span_bench
);
criterion_main!(benches);
