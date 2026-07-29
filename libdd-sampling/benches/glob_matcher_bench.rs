// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Microbenchmarks for `GlobMatcher` covering the `*` short-circuit, ASCII fast path (with and
//! without wildcards, including backtracking), and Unicode fallback path.

use std::alloc::System;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use libdd_common::bench_utils::{
    memory_allocated_measurement, AllocatedBytesMeasurement, ReportingAllocator,
};
use libdd_sampling::glob_matcher::GlobMatcher;

#[global_allocator]
static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

struct BenchCase {
    name: &'static str,
    pattern: &'static str,
    subject: &'static str,
}

fn cases() -> Vec<BenchCase> {
    vec![
        BenchCase {
            name: "star_short_circuit",
            pattern: "*",
            subject: "anything-goes-here",
        },
        BenchCase {
            name: "ascii_exact_match",
            pattern: "my-service",
            subject: "my-service",
        },
        BenchCase {
            name: "ascii_exact_miss",
            pattern: "my-service",
            subject: "other-service",
        },
        BenchCase {
            name: "ascii_case_insensitive_match",
            pattern: "my-service",
            subject: "MY-SERVICE",
        },
        BenchCase {
            name: "ascii_wildcard_star_match",
            pattern: "svc-*",
            subject: "svc-web",
        },
        BenchCase {
            name: "ascii_wildcard_question_match",
            pattern: "svc-???",
            subject: "svc-web",
        },
        BenchCase {
            name: "ascii_wildcard_backtrack_match",
            pattern: "*-controller",
            subject: "users-controller",
        },
        // Worst-case shape for the two-pointer backtracking algorithm.
        BenchCase {
            name: "ascii_wildcard_heavy_backtrack",
            pattern: "a*a*a*a*b",
            subject: "aaaaaaaaaaaaaaaaaaaab",
        },
        BenchCase {
            name: "unicode_pattern_wildcard_match",
            pattern: "caf\u{00e9}-*",
            subject: "CAF\u{00c9}-PAYMENT",
        },
        BenchCase {
            name: "unicode_pattern_ascii_subject",
            pattern: "caf\u{00e9}-*",
            subject: "CAFE-PAYMENT",
        },
        BenchCase {
            name: "ascii_pattern_unicode_subject",
            pattern: "caf*",
            subject: "caf\u{00e9}-controller",
        },
        BenchCase {
            name: "unicode_exact_match",
            pattern: "caf\u{00e9}",
            subject: "CAF\u{00c9}",
        },
    ]
}

fn bench_wall_time(c: &mut Criterion) {
    for case in cases() {
        let matcher = GlobMatcher::new(case.pattern);
        c.bench_function(&format!("glob_matcher/{}/wall_time", case.name), |b| {
            b.iter_batched(
                || (),
                |_| {
                    black_box(matcher.matches(black_box(case.subject)));
                },
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_allocs(c: &mut Criterion<AllocatedBytesMeasurement<System>>) {
    for case in cases() {
        let matcher = GlobMatcher::new(case.pattern);
        c.bench_function(
            &format!("glob_matcher/{}/allocated_bytes", case.name),
            |b| {
                b.iter_batched(
                    || (),
                    |_| {
                        black_box(matcher.matches(black_box(case.subject)));
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

criterion_group!(benches, bench_wall_time);
criterion_group!(
    name = alloc_benches;
    config = memory_allocated_measurement(&GLOBAL);
    targets = bench_allocs
);
criterion_main!(alloc_benches, benches);
