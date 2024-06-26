// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

fn normalize_service_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization");
    let cases = &[
        "#test_starting_hash",
            "TestCAPSandSuch",
            "Test Conversion Of Weird !@#$%^&**() Characters",
            "$#weird_starting",
            "allowed:c0l0ns",
            "1love",
            "ünicöde",
            "ünicöde:metäl",
            "Data🐨dog🐶 繋がっ⛰てて",
            " spaces   ",
            " #hashtag!@#spaces #__<>#  ",
            ":testing",
            "_foo",
            ":::test",
            "contiguous_____underscores",
            "foo_",
            "\u{017F}odd_\u{017F}case\u{017F}",
            "",
            " ",
            "ok",
            "™Ö™Ö™™Ö™",
            "AlsO:ök",
            ":still_ok",
            "___trim",
            "12.:trim@",
            "12.:trim@@",
            "fun:ky__tag/1",
            "fun:ky@tag/2",
            "fun:ky@@@tag/3",
            "tag:1/2.3",
            "---fun:k####y_ta@#g/1_@@#",
            "AlsO:œ#@ö))œk",
            "A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000",
        ];

    group.bench_function("normalize_service", |b| {
        b.iter_batched_ref(
            || cases.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            |cases| {
                for c in cases {
                    datadog_trace_normalization::normalize_utils::normalize_service(c);
                }
            },
            BatchSize::NumIterations(100000),
        )
    });
}

fn normalize_name_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization");
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
}

criterion_group!(benches, normalize_service_bench, normalize_name_bench);
criterion_main!(benches);
