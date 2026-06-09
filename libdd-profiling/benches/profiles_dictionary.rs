// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BatchSize, BenchmarkId, Criterion, Throughput};
use libdd_profiling::profiles::datatypes::ProfilesDictionary;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 16];
const STRINGS_PER_THREAD: usize = 1024;
// Bound one generated function-name component so the input has repeated
// function-like fragments while each full string stays unique.
const FUNCTION_NAME_VARIANTS: usize = 97;
// Knuth/Fibonacci multiplicative hash constant, used only to vary synthetic input.
const KNUTH_MULTIPLICATIVE_HASH: usize = 2_654_435_761;

fn make_strings(thread_count: usize) -> Vec<Vec<String>> {
    (0..thread_count)
        .map(|thread_id| {
            (0..STRINGS_PER_THREAD)
                .map(|string_id| {
                    format!(
                        "/opt/datadog/profiler/thread-{thread_id}/module-{string_id:04}/function-{}::{}",
                        string_id % FUNCTION_NAME_VARIANTS,
                        string_id.wrapping_mul(KNUTH_MULTIPLICATIVE_HASH)
                    )
                })
                .collect()
        })
        .collect()
}

fn insert_profile_strings(dict: &ProfilesDictionary, strings: &[String]) {
    for string in strings {
        black_box(dict.try_insert_str2(string.as_str()).unwrap());
    }
}

fn insert_dictionary_strings_concurrently(strings: &[Vec<String>]) -> ProfilesDictionary {
    let dict = ProfilesDictionary::try_new().unwrap();

    if let [strings] = strings {
        insert_profile_strings(&dict, strings);
        return dict;
    }

    let barrier = Barrier::new(strings.len());
    thread::scope(|scope| {
        for thread_strings in strings {
            let dict = &dict;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                insert_profile_strings(dict, thread_strings);
            });
        }
    });

    dict
}

pub fn bench_profiles_dictionary(c: &mut Criterion) {
    let mut group = c.benchmark_group("profiles_dictionary/unique_string_inserts");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);

    for thread_count in THREAD_COUNTS {
        let strings = make_strings(thread_count);
        let total_strings = thread_count * STRINGS_PER_THREAD;
        group.throughput(Throughput::Elements(total_strings as u64));
        group.bench_with_input(
            BenchmarkId::new("threads", thread_count),
            &strings,
            |b, strings| {
                b.iter_batched(
                    || strings,
                    |strings| black_box(insert_dictionary_strings_concurrently(strings)),
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_profiles_dictionary);
