// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Micro benchmarks for [`DDSketch`].
//!
//! `DDSketch` is the streaming quantile histogram used to summarize trace latency
//! distributions for APM stats and to aggregate metric distributions. `add` /
//! `add_with_count` are called in a tight per-value loop on the hot stats path, while
//! `into_pb` / `encode_to_vec` are on the IPC/stats export path and `ordered_bins` /
//! `count` are used when reading sketches back for metrics export.
//!
//! Data is drawn from a seeded RNG (fixed seed, no clock) so runs are comparable, and the
//! values are pre-generated into an array before timing so the generation cost is excluded
//! from the measured loop. Values model latencies expressed in nanoseconds, the unit
//! actually fed to the sketch on the trace-stats path (`add(duration as f64)`).

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use libdd_ddsketch::DDSketch;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Number of points pushed per `add`/`add_with_count` benchmark iteration. A batch keeps
/// criterion measuring a loop of the (very fast) per-value operation rather than a single
/// call, giving stable timings.
const ADD_BATCH: usize = 4096;

/// Fixed RNG seed so the pre-generated sample arrays are identical across runs and the
/// benchmarks stay comparable.
const RNG_SEED: u64 = 0xD5D5_E7C4;

/// A named value distribution feeding the sketch.
///
/// `sample` draws a single value (in nanoseconds) from the supplied RNG. Drawing from a
/// seeded [`ChaCha8Rng`] keeps the produced data deterministic and reproducible across runs
/// (unlike `StdRng`, the ChaCha generator's algorithm is stable across `rand` releases).
struct Distribution {
    name: &'static str,
    sample: fn(&mut ChaCha8Rng) -> f64,
}

/// Latencies clustered in the sub-millisecond range (~1us..~1ms). Representative of fast,
/// homogeneous operations (cache hits, in-memory work). All values land in a narrow band of
/// bins, so the store barely grows and never collapses.
fn clustered_near_zero(rng: &mut ChaCha8Rng) -> f64 {
    rng.gen_range(1_000.0..1_000_000.0)
}

/// Large latencies forcing high bin indices (~1ms..~10s). Representative of slow endpoints
/// / batch jobs. Wider than the clustered case but still within `max_size` bins (no
/// collapse).
fn large_values(rng: &mut ChaCha8Rng) -> f64 {
    rng.gen_range(1_000_000.0..1e10)
}

/// Mixed latencies spread log-uniformly across 1us..1s, the typical shape of a real
/// service's latency stream. Spans many bins but stays under `max_size`.
fn mixed(rng: &mut ChaCha8Rng) -> f64 {
    let exp = rng.gen_range(3.0..9.0); // 1e3 .. 1e9 ns
    10f64.powf(exp)
}

/// Extremely wide spread (1ns..~1e16ns) that forces the store past `max_size` (2048),
/// repeatedly triggering low-bin collapse. Pathological but exercises the collapse path
/// that is otherwise never hit; useful to track its cost in isolation.
fn collapsing(rng: &mut ChaCha8Rng) -> f64 {
    let exp = rng.gen_range(0.0..16.0); // 1 .. 1e16
    10f64.powf(exp)
}

const DISTRIBUTIONS: &[Distribution] = &[
    Distribution {
        name: "clustered_near_zero",
        sample: clustered_near_zero,
    },
    Distribution {
        name: "large_values",
        sample: large_values,
    },
    Distribution {
        name: "mixed",
        sample: mixed,
    },
    Distribution {
        name: "collapsing",
        sample: collapsing,
    },
];

/// Pre-generate `n` values from `dist` using a freshly seeded RNG, so the sampling cost is
/// paid once up front and never enters the timed benchmark loop.
fn samples(dist: &Distribution, n: usize) -> Vec<f64> {
    let mut rng = ChaCha8Rng::seed_from_u64(RNG_SEED);
    (0..n).map(|_| (dist.sample)(&mut rng)).collect()
}

/// Benchmark `add` / `add_with_count` across the value distributions.
///
/// Each iteration pushes [`ADD_BATCH`] points into a fresh sketch so that the cost measured
/// is the per-value `LogMapping::index` math plus the store growth / collapse, amortized
/// over a realistic batch.
fn bench_add(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddsketch_add");
    group.throughput(Throughput::Elements(ADD_BATCH as u64));

    for dist in DISTRIBUTIONS {
        let values = samples(dist, ADD_BATCH);

        group.bench_with_input(BenchmarkId::new("add", dist.name), &values, |b, values| {
            b.iter_batched(
                DDSketch::default,
                |mut sketch| {
                    for &v in values {
                        let _ = black_box(sketch.add(black_box(v)));
                    }
                    sketch
                },
                BatchSize::SmallInput,
            )
        });

        group.bench_with_input(
            BenchmarkId::new("add_with_count", dist.name),
            &values,
            |b, values| {
                b.iter_batched(
                    DDSketch::default,
                    |mut sketch| {
                        for (i, &v) in values.iter().enumerate() {
                            // `add_with_count` is reached via the FFI surface
                            // (`ddog_ddsketch_add_with_count`); vary the count so the bench also
                            // covers the weight != 1 path rather than reducing to plain `add`.
                            let count = 1.0 + (i % 8) as f64;
                            let _ =
                                black_box(sketch.add_with_count(black_box(v), black_box(count)));
                        }
                        sketch
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

/// Number of points seeded into a sketch for the read/serialize benchmarks. A busy 10s stats
/// bucket aggregates on the order of thousands of durations per `(resource, service, ...)`
/// group, so 10k is a representative-to-busy fill.
const SEED_POINTS: usize = 10_000;

/// Build a sketch pre-filled with [`SEED_POINTS`] points drawn from `dist`.
///
/// The cost of `into_pb`/`encode_to_vec`/`ordered_bins`/`count` scales with the length of the
/// contiguous bin vector (including interior empty bins), which is driven by how *wide* the
/// distribution spreads, not by the point count. Seeding different distributions therefore
/// produces meaningfully different vector lengths: `clustered_near_zero` and `mixed` populate
/// a moderate span of bins, while `collapsing` saturates at `max_size` (2048).
fn seeded_sketch(dist: &Distribution) -> DDSketch {
    let mut sketch = DDSketch::default();
    for v in samples(dist, SEED_POINTS) {
        let _ = sketch.add(v);
    }
    sketch
}

/// Benchmark protobuf serialization (`into_pb`, `encode_to_vec`) across distributions, which
/// vary the populated bin-vector length.
///
/// Both consume `self`, so a fresh clone is built per iteration via `iter_batched`; the
/// clone is excluded from the timed routine.
fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddsketch_encode");

    for dist in DISTRIBUTIONS {
        let sketch = seeded_sketch(dist);

        group.bench_with_input(BenchmarkId::new("into_pb", dist.name), &sketch, |b, s| {
            b.iter_batched(
                || s.clone(),
                |s| black_box(s.into_pb()),
                BatchSize::SmallInput,
            )
        });

        group.bench_with_input(
            BenchmarkId::new("encode_to_vec", dist.name),
            &sketch,
            |b, s| {
                b.iter_batched(
                    || s.clone(),
                    |s| black_box(s.encode_to_vec()),
                    BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

/// Benchmark the O(n-bins) read paths (`ordered_bins`, `count`) used on metrics export,
/// across distributions (varying the populated bin-vector length).
fn bench_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddsketch_read");

    for dist in DISTRIBUTIONS {
        let sketch = seeded_sketch(dist);

        group.bench_with_input(
            BenchmarkId::new("ordered_bins", dist.name),
            &sketch,
            |b, s| b.iter(|| black_box(s.ordered_bins())),
        );

        group.bench_with_input(BenchmarkId::new("count", dist.name), &sketch, |b, s| {
            b.iter(|| black_box(s.count()))
        });
    }

    group.finish();
}

criterion_group!(benches, bench_add, bench_encode, bench_read);
criterion_main!(benches);
