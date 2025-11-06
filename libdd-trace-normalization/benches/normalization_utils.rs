// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::measurement::WallTime;
use criterion::Throughput::Elements;
use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkGroup, BenchmarkId, Criterion,
};
use libdd_trace_normalization::normalize_utils::{normalize_name, normalize_service};
use libdd_trace_normalization::normalizer::normalize_trace;
use libdd_trace_protobuf::pb;
use std::hint::black_box;
use std::{collections::HashMap, time::Duration};

fn normalize_service_bench(c: &mut Criterion) {
    let group = c.benchmark_group("normalization/normalize_service");
    let cases = &[
            "",
            "test_ASCII",
            "Test Conversion 0f Weird !@#$%^&**() Characters",
            "Data🐨dog🐶 繋がっ⛰てて",
            "A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000",
        ];

    normalize_fnmut_string(group, cases, 1000, "normalize_service", normalize_service);
}

fn normalize_name_bench(c: &mut Criterion) {
    let group = c.benchmark_group("normalization/normalize_name");
    let cases = &[
        "good",
        "bad-name",
        "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
    ];
    normalize_fnmut_string(group, cases, 1000, "normalize_name", normalize_name);
}

#[inline]
fn normalize_fnmut_string<F>(
    mut group: BenchmarkGroup<WallTime>,
    cases: &[&str],
    elements: usize,
    function_name: &str,
    mut function: F,
) where
    F: FnMut(&mut String),
{
    // Measure over a number of calls to minimize impact of OS noise
    group.throughput(Elements(elements as u64));
    // We only need to measure for a small time since the function is very fast
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(200);
    group.sampling_mode(criterion::SamplingMode::Flat);

    for case in cases {
        group.bench_with_input(
            BenchmarkId::new(
                function_name,
                if case.is_empty() {
                    "[empty string]"
                } else {
                    case
                },
            ),
            *case,
            |b, case| {
                b.iter_batched_ref(
                    || {
                        let mut strings = Vec::with_capacity(elements);
                        (0..elements).for_each(|_| strings.push(case.to_owned()));
                        strings
                    },
                    |strings| {
                        #[allow(clippy::unit_arg)]
                        strings.iter_mut().for_each(|string| {
                            black_box(function(black_box(string)));
                        });
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    group.finish();
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
            span_events: vec![],
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
            span_events: vec![],
        },
    ];

    c.bench_with_input(
        BenchmarkId::new("normalization/normalize_trace", "test_trace"),
        &trace,
        |b, case| {
            b.iter_batched_ref(
                || case.to_owned(),
                |t| black_box(normalize_trace(black_box(t))),
                BatchSize::LargeInput,
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
