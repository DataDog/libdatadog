// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

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
            BenchmarkId::new("normalize_service", case),
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

criterion_group!(benches, normalize_service_bench, normalize_name_bench);
criterion_main!(benches);
