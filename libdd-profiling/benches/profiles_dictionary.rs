// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BatchSize, BenchmarkId, Criterion, Throughput};
use libdd_profiling::profiles::datatypes::ProfilesDictionary;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

// 1-2 threads matches expected profiler usage; higher counts stress contention behavior.
const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 16];
const STRINGS_PER_THREAD: usize = 1024;
// Bound one generated function-name component so the input has repeated
// function-like fragments, with some full strings shared across workers.
const FUNCTION_NAME_VARIANTS: usize = 97;
const STRING_SHAPE_VARIANTS: usize = 4;
// Knuth/Fibonacci multiplicative hash constant, used only to vary synthetic input.
const KNUTH_MULTIPLICATIVE_HASH: usize = 2_654_435_761;

// The outer Vec partitions precomputed input by benchmark worker thread; each
// inner Vec is the set of strings inserted by one worker.
fn make_partitioned_strings(thread_count: usize) -> Vec<Vec<String>> {
    (0..thread_count)
        .map(|thread_id| {
            (0..STRINGS_PER_THREAD)
                .map(|string_id| {
                    let function_id = string_id % FUNCTION_NAME_VARIANTS;
                    let mixed_id = string_id.wrapping_mul(KNUTH_MULTIPLICATIVE_HASH);

                    match string_id % STRING_SHAPE_VARIANTS {
                        0 => format!("function_{function_id}::{mixed_id}"),
                        1 => format!("/src/thread_{thread_id}/module_{function_id}/file_{string_id:04}.rs"),
                        2 => {
                            format!("datadog::profiling::module_{function_id}::function_{mixed_id}")
                        }
                        _ => format!(
                            "/opt/datadog/profiler/thread-{thread_id}/module-{function_id}/src/file_{string_id:04}.rs::function_{function_id}::{mixed_id}",
                        ),
                    }
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

pub fn bench_profile_string_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("profiles_dictionary/profile_string_inserts");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);

    for thread_count in THREAD_COUNTS {
        // Precompute input outside the measured closure so the benchmark measures
        // dictionary insertion rather than string formatting/allocation.
        let strings = make_partitioned_strings(thread_count);
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

criterion_group!(benches, bench_profile_string_inserts);
