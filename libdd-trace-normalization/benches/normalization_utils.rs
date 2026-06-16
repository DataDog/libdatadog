// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::measurement::WallTime;
use criterion::Throughput::Elements;
use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkGroup, BenchmarkId, Criterion,
};
use libdd_trace_normalization::normalize_utils::{
    normalize_metric_name_bench_wrapper, normalize_name, normalize_service,
    normalize_span_start_duration, normalize_tag, truncate_utf8_bench_wrapper,
};
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

/// `normalize_tag` runs on every ingested tag key/value. It is the heaviest normalization
/// function: a nested loop combining an ASCII fast-path with per-codepoint UTF-8 scanning and a
/// char-class state machine. We exercise realistic tag values plus the unicode and over-length
/// paths that defeat the ASCII fast-path.
fn normalize_tag_bench(c: &mut Criterion) {
    let group = c.benchmark_group("normalization/normalize_tag");
    let cases = &[
        // Empty input: measures the early-return baseline.
        "",
        // Already-clean realistic tag values: ASCII fast-path only.
        "ascii:http.method:get",
        "ascii:env:production",
        "ascii:resource:get_/api/v1/users/{id}",
        // Mixed: needs the illegal-char state machine but stays ASCII.
        "mixed:Some Service Name!!",
        // Unicode service name: exercises the codepoint-scanning slow path.
        "unicode:café-Über-Sérvice",
        "unicode:Data🐨dog🐶 繋がっ⛰てて",
        // Over-length (> MAX_TAG_LEN = 200): forces the loop to run to the codepoint cap.
        "over-length-ascii:over_length_ascii_value_that_keeps_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going_and_going",
    ];
    normalize_fnmut_string(group, cases, 1000, "normalize_tag", normalize_tag);
}

/// `normalize_metric_name` runs on every span name. Similar complexity to `normalize_tag` with a
/// one-byte lookahead (`last_written_char`) to collapse separators.
fn normalize_metric_name_bench(c: &mut Criterion) {
    let group = c.benchmark_group("normalization/normalize_metric_name");
    let cases = &[
        // Empty input: measures the early-return baseline.
        "",
        // Already-clean span names.
        "http.request",
        "django.controller",
        // Names needing separator collapsing / illegal-char replacement.
        "GET /some/raclette",
        "rails.action_controller.process",
        // Over-length (> MAX_NAME_LEN = 100).
        "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
    ];
    normalize_fnmut_string(
        group,
        cases,
        1000,
        "normalize_metric_name",
        normalize_metric_name_bench_wrapper,
    );
}

/// `truncate_utf8` is called before every name/service/type normalization to enforce a byte
/// limit while preserving UTF-8 boundaries. We bench the over-length cases (where it actually does
/// work) at the real limits used in the code, including a multi-byte boundary that must be walked
/// back.
fn truncate_utf8_bench(c: &mut Criterion) {
    let group = c.benchmark_group("normalization/truncate_utf8");
    // MAX_SERVICE_LEN / MAX_NAME_LEN / MAX_TYPE_LEN are all 100 in the source.
    const LIMIT: usize = 100;
    let ascii_over = "a".repeat(256);
    // Multi-byte chars (3 bytes each) so the limit falls mid-codepoint and must be walked back.
    let unicode_over = "繋".repeat(128);
    let cases: &[(&str, &str)] = &[
        ("over-length-ascii", ascii_over.as_str()),
        ("over-length-unicode", unicode_over.as_str()),
    ];

    normalize_fnmut_string_with(
        group,
        cases,
        1000,
        "truncate_utf8",
        move |s: &mut String| truncate_utf8_bench_wrapper(s, LIMIT),
    );
}

/// `normalize_span_start_duration` runs on every span and, in the common case where the start
/// timestamp predates the year-2000 cutoff, performs a `SystemTime` read. We bench in a tight loop
/// to confirm that read isn't a meaningful per-span tax. The "clean" case skips the clock; the
/// "needs-clock" case forces the `SystemTime::elapsed()` path.
fn normalize_span_start_duration_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization/normalize_span_start_duration");
    // Each measured iteration normalizes a batch of `ELEMENTS` spans so the per-span cost (a few
    // integer ops, or a `SystemTime` read on the year-2000 path) isn't swamped by timer overhead.
    // The batch is rebuilt fresh in (untimed) setup because the function mutates its inputs in
    // place: on the "needs-clock" path the first call rewrites `start` to a recent timestamp, which
    // would make a second call on the same value skip the clock branch.
    const ELEMENTS: usize = 1000;
    group.throughput(Elements(ELEMENTS as u64));
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(200);
    group.sampling_mode(criterion::SamplingMode::Flat);

    // (start, duration): valid recent timestamp (no clock read) vs. a too-old start that forces
    // the SystemTime read.
    let cases: &[(&str, i64, i64)] = &[
        ("clean", 1_448_466_874_000_000_000, 10_000_000),
        ("needs-clock", 0, 10_000_000),
    ];

    for (label, start, duration) in cases {
        group.bench_with_input(
            BenchmarkId::new("normalize_span_start_duration", label),
            &(*start, *duration),
            |b, &(start, duration)| {
                b.iter_batched_ref(
                    || vec![(start, duration); ELEMENTS],
                    |pairs| {
                        for (s, d) in pairs {
                            normalize_span_start_duration(black_box(s), black_box(d));
                        }
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    group.finish();
}

/// Like [`normalize_fnmut_string`] but takes labelled cases (label, input) so over-length inputs
/// don't need to be displayed verbatim in benchmark ids.
#[inline]
fn normalize_fnmut_string_with<F>(
    mut group: BenchmarkGroup<WallTime>,
    cases: &[(&str, &str)],
    elements: usize,
    function_name: &str,
    mut function: F,
) where
    F: FnMut(&mut String),
{
    group.throughput(Elements(elements as u64));
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(200);
    group.sampling_mode(criterion::SamplingMode::Flat);

    for (label, case) in cases {
        group.bench_with_input(BenchmarkId::new(function_name, label), *case, |b, case| {
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
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    normalize_service_bench,
    normalize_name_bench,
    normalize_span_bench,
    normalize_tag_bench,
    normalize_metric_name_bench,
    truncate_utf8_bench,
    normalize_span_start_duration_bench
);
criterion_main!(benches);
