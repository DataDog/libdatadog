// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

fn normalize_service_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization");
    let cases = &[
        ("#test_starting_hash", "test_starting_hash"),
            ("TestCAPSandSuch", "testcapsandsuch"),
            (
                "Test Conversion Of Weird !@#$%^&**() Characters",
                "test_conversion_of_weird_characters",
            ),
            ("$#weird_starting", "weird_starting"),
            ("allowed:c0l0ns", "allowed:c0l0ns"),
            ("1love", "love"),
            ("ünicöde", "ünicöde"),
            ("ünicöde:metäl", "ünicöde:metäl"),
            ("Data🐨dog🐶 繋がっ⛰てて", "data_dog_繋がっ_てて"),
            (" spaces   ", "spaces"),
            (" #hashtag!@#spaces #__<>#  ", "hashtag_spaces"),
            (":testing", ":testing"),
            ("_foo", "foo"),
            (":::test", ":::test"),
            ("contiguous_____underscores", "contiguous_underscores"),
            ("foo_", "foo"),
            (
                "\u{017F}odd_\u{017F}case\u{017F}",
                "\u{017F}odd_\u{017F}case\u{017F}",
            ), // edge-case
            ("", ""),
            (" ", ""),
            ("ok", "ok"),
            ("™Ö™Ö™™Ö™", "ö_ö_ö"),
            ("AlsO:ök", "also:ök"),
            (":still_ok", ":still_ok"),
            ("___trim", "trim"),
            ("12.:trim@", ":trim"),
            ("12.:trim@@", ":trim"),
            ("fun:ky__tag/1", "fun:ky_tag/1"),
            ("fun:ky@tag/2", "fun:ky_tag/2"),
            ("fun:ky@@@tag/3", "fun:ky_tag/3"),
            ("tag:1/2.3", "tag:1/2.3"),
            ("---fun:k####y_ta@#g/1_@@#", "fun:k_y_ta_g/1"),
            ("AlsO:œ#@ö))œk", "also:œ_ö_œk"),
            (
                "A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000",
                "a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000_0"
            ),
        ];

    group.bench_function("normalize_service", |b| {
        b.iter_batched_ref(
            || cases.iter().map(|(c, _)| c.to_string()).collect::<Vec<_>>(),
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
