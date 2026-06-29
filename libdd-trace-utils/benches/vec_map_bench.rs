// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Microbenchmarks for [`VecMap`], the linear-scan ordered map backing some of span's associative
//! maps.
//!
//! Keys are [`BytesString`] to match real span usage (`meta`/`metrics` are keyed by `BytesString`).
//! Map sizes span the typical range up to a large end (128). We expect the advantage of `VecMap` to
//! degrade with size and with duplicates rate.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::vec_map::VecMap;
use std::hint::black_box;

/// Representative map sizes: the small end is the common case (a span carries a handful of tags),
/// the larger end covers heavily-tagged spans. Deliberately bounded — `VecMap` is never expected
/// to hold thousands of entries.
const SIZES: &[usize] = &[8, 16, 64, 128];

/// A small set of prefixes resembling real span meta namespaces. Includes an empty prefix so that
/// not every key shares a common head — keys generated from different prefixes diverge on the very
/// first byte, which is the realistic mix the linear scan actually sees.
const PREFIXES: &[&str] = &["", "http.", "db.", "aws.", "_dd."];

/// Duplicate periods exercised by the dedup benches: a key is re-inserted (shadowed) every
/// `period`-th insert, so the duplicate rate is `1/period`. We cover 50% (2, unrealistic/worse
/// case), 25% (4) and 10% (10) to measure how dedup cost scales with duplicate rates.
const DUP_PERIODS: &[usize] = &[2, 4, 10];

/// Build a deterministic set of `BytesString` keys shaped like real span tag names.
fn keys(n: usize) -> Vec<BytesString> {
    // Dotted names resembling real span meta keys (`http.method`, `db.statement`, ...). Generated
    // deterministically. Prefixes are picked by modulo over a the `PREFIXES` set.
    // The index is put first to simulate the fact that after the prefix, the identifiers are likely
    // to be distinct. Doing the converse would add a longer common prefix.
    (0..n)
        .map(|i| {
            let prefix = PREFIXES[i % PREFIXES.len()];
            BytesString::from_string(format!("{prefix}{i:03}-nth-key"))
        })
        .collect()
}

/// Build deterministic string values, sized like typical meta values.
fn values(n: usize) -> Vec<BytesString> {
    (0..n)
        .map(|i| BytesString::from_string(format!("value-{i:03}")))
        .collect()
}

/// A pre-populated `meta`-shaped map (`BytesString -> BytesString`) with `n` unique keys.
fn populated_meta(n: usize) -> VecMap<BytesString, BytesString> {
    keys(n).into_iter().zip(values(n)).collect()
}

/// A `metrics`-shaped map (`BytesString -> f64`) with `n` unique keys.
fn populated_metrics(n: usize) -> VecMap<BytesString, f64> {
    keys(n)
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, i as f64))
        .collect()
}

/// Insert: builds a fresh map of `n` entries from scratch (the construction path on the client's
/// hot path). `insert` mutates, so we rebuild the input each iteration with `iter_batched`.
fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/insert");

    for &n in SIZES {
        group.throughput(criterion::Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let ks = keys(n);
            let vs = values(n);
            b.iter_batched(
                || (ks.clone(), vs.clone()),
                |(ks, vs)| {
                    let mut map = VecMap::with_capacity(n);
                    for (k, v) in ks.into_iter().zip(vs) {
                        map.insert(black_box(k), black_box(v));
                    }
                    map
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// Get (hit): looks up every present key once, reporting the average successful-lookup cost.
/// `get` returns the last match (scanning from the back), so this averages over scan distances.
fn bench_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/get_hit");

    for &n in SIZES {
        group.throughput(criterion::Throughput::Elements(n as u64));
        let map = populated_meta(n);
        let lookups = keys(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for k in &lookups {
                    black_box(map.get(black_box(k.as_str())));
                }
            })
        });
    }
    group.finish();
}

/// Get (miss): worst case for a linear-scan map — a full scan that finds nothing.
fn bench_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/get_miss");

    for &n in SIZES {
        let map = populated_meta(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                black_box(map.get(black_box("this.key.is.absent")));
            })
        });
    }
    group.finish();
}

/// Get_mut (hit): mutable lookup of every key.
fn bench_get_mut(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/get_mut");

    for &n in SIZES {
        group.throughput(criterion::Throughput::Elements(n as u64));
        let lookups = keys(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched_ref(
                || populated_metrics(n),
                |map| {
                    for k in &lookups {
                        if let Some(v) = map.get_mut(black_box(k.as_str())) {
                            *v += 1.0;
                        }
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// Contains_key: full scan (`any`), checking every present key plus one absent key.
fn bench_contains_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/contains_key");

    for &n in SIZES {
        group.throughput(criterion::Throughput::Elements(n as u64));
        let map = populated_meta(n);
        let lookups = keys(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for k in &lookups {
                    black_box(map.contains_key(black_box(k.as_str())));
                }
                black_box(map.contains_key(black_box("this.key.is.absent")));
            })
        });
    }
    group.finish();
}

/// Iter: full traversal, as performed on the encode path.
fn bench_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/iter");

    for &n in SIZES {
        group.throughput(criterion::Throughput::Elements(n as u64));
        let map = populated_meta(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for (k, v) in map.iter() {
                    black_box((k, v));
                }
            })
        });
    }
    group.finish();
}

/// A `meta`-shaped map where roughly one in `period` of the inserts is a duplicate key (a tag being
/// overwritten). This is the realistic "has duplicates" shape that `dedup` has to compact; a
/// smaller `period` means more duplicates.
fn meta_with_duplicates(n: usize, period: usize) -> VecMap<BytesString, BytesString> {
    let mut map = VecMap::with_capacity(n + n / period);

    for (i, (k, v)) in keys(n).into_iter().zip(values(n)).enumerate() {
        // Re-insert every `period`-th key first to create a duplicate (the earlier value gets
        // shadowed).
        if i % period == 0 {
            map.insert(k.clone(), BytesString::from_static("stale"));
        }
        map.insert(k, v);
    }

    map
}

/// dedup(): runs once per span on decode. `dedup` mutates and sets a flag, so we rebuild the
/// (un-deduped) input each iteration. Benched both with and without duplicates.
fn bench_dedup(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/dedup");

    for &n in SIZES {
        group.bench_with_input(BenchmarkId::new("no_duplicates", n), &n, |b, &n| {
            b.iter_batched_ref(
                || populated_meta(n),
                |map| {
                    map.dedup();
                    black_box(&*map);
                },
                BatchSize::SmallInput,
            )
        });
        for &period in DUP_PERIODS {
            group.bench_with_input(
                BenchmarkId::new(format!("dup_1_in_{period}"), n),
                &n,
                |b, &n| {
                    b.iter_batched_ref(
                        || meta_with_duplicates(n, period),
                        |map| {
                            map.dedup();
                            black_box(&*map);
                        },
                        BatchSize::SmallInput,
                    )
                },
            );
        }
    }
    group.finish();
}

/// as_deduped_map(): the immutable variant used on the encode path. When the map is already deduped
/// it borrows for free; when not, it dedup on the fly with a side allocation. Both cases are
/// benched, and iterated through.
fn bench_as_deduped_map(c: &mut Criterion) {
    let mut group = c.benchmark_group("vec_map/as_deduped_map");

    for &n in SIZES {
        // Already-deduped: cheap borrow path (the common case on encode).
        let mut deduped = populated_meta(n);
        deduped.dedup();
        group.bench_with_input(BenchmarkId::new("already_deduped", n), &n, |b, _| {
            b.iter(|| {
                let map = black_box(deduped.as_deduped_map());
                for (k, v) in map.iter() {
                    black_box((k, v));
                }
            })
        });

        // Not deduped, with duplicates: allocating fallback path.
        for &period in DUP_PERIODS {
            let dirty = meta_with_duplicates(n, period);
            group.bench_with_input(
                BenchmarkId::new(format!("needs_dedup_1_in_{period}"), n),
                &n,
                |b, _| {
                    b.iter(|| {
                        let map = black_box(dirty.as_deduped_map());
                        for (k, v) in map.iter() {
                            black_box((k, v));
                        }
                    })
                },
            );
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert,
    bench_get_hit,
    bench_get_miss,
    bench_get_mut,
    bench_contains_key,
    bench_iter,
    bench_dedup,
    bench_as_deduped_map,
);
criterion_main!(benches);
