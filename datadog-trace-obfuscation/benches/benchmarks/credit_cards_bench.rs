// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::hint::black_box;
use std::time::Duration;

use criterion::Throughput::Elements;
use criterion::{criterion_group, BatchSize, BenchmarkId, Criterion};
use datadog_trace_obfuscation::credit_cards::is_card_number;

pub fn is_card_number_bench(c: &mut Criterion) {
    bench_is_card_number(c, "is_card_number", true);
}

fn is_card_number_no_luhn_bench(c: &mut Criterion) {
    bench_is_card_number(c, "is_card_number_no_luhn", false);
}

#[inline(always)]
fn bench_is_card_number(c: &mut Criterion, function_name: &str, validate_luhn: bool) {
    let mut group = c.benchmark_group("credit_card");
    // Measure over a number of calls to minimize impact of OS noise
    let elements = 1000;
    group.throughput(Elements(elements));
    // We only need to measure for a small time since the function is very fast
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sampling_mode(criterion::SamplingMode::Flat);
    group.sample_size(200);
    let ccs = [
        "378282246310005",
        "  378282246310005",
        "  3782-8224-6310-005 ",
        "37828224631000521389798", // valid but too long
        "37828224631",             // valid but too short
        "x371413321323331",        // invalid characters
        "",
    ];
    for c in ccs.iter() {
        group.bench_with_input(BenchmarkId::new(function_name, c), c, |b, i| {
            b.iter_batched(
                || {},
                |_| {
                    for _ in 0..elements {
                        black_box(is_card_number_uninlined(i, validate_luhn));
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
}

#[inline(never)]
fn is_card_number_uninlined<T: AsRef<str>>(s: T, validate_luhn: bool) -> bool {
    black_box(is_card_number(black_box(s), black_box(validate_luhn)))
}

criterion_group!(benches, is_card_number_bench, is_card_number_no_luhn_bench);
