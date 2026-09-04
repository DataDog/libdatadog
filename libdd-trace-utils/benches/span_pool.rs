// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::mpsc;
use std::thread;

use criterion::measurement::Measurement;
use criterion::{black_box, criterion_group, Criterion, Throughput};
use libdd_common::bench_utils::{memory_allocated_measurement, MeasurementName};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::span_pool::SpanPool;
use libdd_trace_utils::span::v04::Span;
use libdd_trace_utils::span::BytesData;

/// Configurations exercised by the benchmarks: `(number of chunks, spans per chunk)`.
/// The small one resembles a single trace flush; the large one resembles a full payload.
const CONFIGS: &[(usize, usize)] = &[(1, 10), (20, 100)];

/// Configuration for the concurrent benchmarks: `(number of chunks, spans per chunk)`.
///
/// Each iteration spawns and joins the worker threads, so the per-iteration thread-spawn cost
/// is part of the measurement. We use a large batch so that cost is amortized across many spans
/// in the per-span throughput (`Throughput::Elements`): the `thrpt` number is what isolates the
/// pool operations from the spawn overhead.
const CONCURRENT_CONFIG: (usize, usize) = (20, 1_000);

/// Worker thread counts exercised by the concurrent benchmarks. All divide the concurrent
/// total (`20 * 100 = 2000`) evenly so each worker builds the same number of spans.
const NUM_WORKERS: &[usize] = &[2, 8];

/// Pool capacity: large enough to hold a full iteration's worth of recycled spans, so the
/// bounded channel never drops spans because it is full (only the drop policy does).
const POOL_CAPACITY: usize = 4_096;

/// Populate a span with realistic fields and a few meta/metrics entries.
///
/// Inserting into `meta`/`metrics` is where the pool pays off: a recycled span keeps its
/// `VecMap`'s backing `Vec` capacity, so the inserts reuse it instead of growing a fresh
/// allocation from zero.
fn populate_span(span: &mut Span<BytesData>, span_idx: u64, trace_id: u128) {
    span.service = BytesString::from_static("test-service");
    span.name = BytesString::from_static("http.request");
    span.resource = BytesString::from_static("GET /api/resource");
    span.r#type = BytesString::from_static("http");
    span.trace_id = trace_id;
    span.span_id = span_idx;
    span.parent_id = if span_idx == 0 { 0 } else { span_idx - 1 };
    span.start = 1_000_000 + span_idx as i64;
    span.duration = 5_000;
    span.error = 0;
    span.meta.insert(
        BytesString::from_static("http.method"),
        BytesString::from_static("GET"),
    );
    span.meta.insert(
        BytesString::from_static("http.route"),
        BytesString::from_static("/api/echo"),
    );
    span.meta.insert(
        BytesString::from_static("http.status_code"),
        BytesString::from_static("200"),
    );
    span.meta.insert(
        BytesString::from_static("_dd.p.dm"),
        BytesString::from_static("-0"),
    );
    span.meta.insert(
        BytesString::from_static("language"),
        BytesString::from_static("python"),
    );
    span.meta.insert(
        BytesString::from_static("runtime-id"),
        BytesString::from_static("bcc8589f1d534d2abf2bd7eb4a8eba2d"),
    );
    span.metrics
        .insert(BytesString::from_static("_sampling_priority_v1"), 2.0);
    span.metrics
        .insert(BytesString::from_static("_dd.top_level"), 1.0);
    span.metrics
        .insert(BytesString::from_static("_dd.tracer_kr"), 1.0);
    span.metrics
        .insert(BytesString::from_static("process_id"), 80474.0);
}

/// Build `num_chunks` chunks of `spans_per_chunk` spans, populating each span via
/// [`populate_span`]. `get_span` decides whether spans come from the pool or are freshly
/// allocated; `pull_empty_chunk` supplies the backing `Vec` (recycled by the pooled path).
fn build_chunks<F: FnMut() -> Span<BytesData>, G: FnMut() -> Vec<Span<BytesData>>>(
    num_chunks: usize,
    spans_per_chunk: usize,
    mut get_span: F,
    mut pull_empty_chunk: G,
) -> Vec<Vec<Span<BytesData>>> {
    let mut chunks = Vec::with_capacity(num_chunks);
    for chunk_idx in 0..num_chunks {
        let trace_id = (chunk_idx as u128) << 64 | chunk_idx as u128;
        let mut chunk = pull_empty_chunk();
        chunk.reserve(spans_per_chunk);
        let base = chunk_idx * spans_per_chunk;
        for i in 0..spans_per_chunk {
            let mut span = get_span();
            populate_span(&mut span, base as u64 + i as u64, trace_id);
            chunk.push(span);
        }
        chunks.push(chunk);
    }
    chunks
}

/// Build a single chunk of `spans` spans, populating each span via [`populate_span`].
/// `get_span` decides whether spans come from the pool or are freshly allocated; `pull_empty_chunk`
/// supplies the backing `Vec` (recycled by the pooled path). Used by the concurrent benchmarks
/// where each worker thread builds one chunk.
fn build_chunk<F: FnMut() -> Span<BytesData>, G: FnMut() -> Vec<Span<BytesData>>>(
    spans: usize,
    base: u64,
    trace_id: u128,
    mut get_span: F,
    mut pull_empty_chunk: G,
) -> Vec<Span<BytesData>> {
    let mut chunk = pull_empty_chunk();
    chunk.reserve(spans);
    for i in 0..spans {
        let mut span = get_span();
        populate_span(&mut span, base + i as u64, trace_id);
        chunk.push(span);
    }
    chunk
}

/// Warm the pool with `count` freshly-allocated, populated spans so that the first
/// measured iterations dequeue recycled spans (steady state) rather than allocating.
fn warm_pool(pool: &SpanPool<BytesData>, count: usize) {
    let chunks = build_chunks(1, count, Span::<BytesData>::default, Vec::new);
    // Returning the chunks to the pool recycles the spans (minus the ~10% dropped by the
    // drop policy).
    drop(pool.wrap_chunks(chunks));
}

/// Dequeue spans from the pool, populate them, build chunks, then enqueue the chunks back
/// into the pool (recycling the spans on drop). Throughput is reported per span.
fn enqueue_dequeue_pooled<M: Measurement + MeasurementName + 'static>(c: &mut Criterion<M>) {
    let mut group = c.benchmark_group(format!("span_pool/{}", M::name()));
    for &(num_chunks, spans_per_chunk) in CONFIGS {
        let total = num_chunks * spans_per_chunk;
        let pool = SpanPool::<BytesData>::new(POOL_CAPACITY);
        warm_pool(&pool, total);

        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(
            format!("{num_chunks}x{spans_per_chunk}/pooled"),
            &(num_chunks, spans_per_chunk),
            |b, &(num_chunks, spans_per_chunk)| {
                b.iter(|| {
                    let chunks = build_chunks(
                        num_chunks,
                        spans_per_chunk,
                        || pool.get_span(),
                        || pool.pull_empty_chunk(),
                    );
                    // Enqueue: returning the chunks to the pool recycles the spans.
                    drop(pool.wrap_chunks(black_box(chunks)));
                });
            },
        );
    }
    group.finish();
}

/// Allocate fresh spans every iteration, populate them, build chunks, then drop the chunks
/// (no recycling — spans are deallocated). Throughput is reported per span.
fn enqueue_dequeue_allocating<M: Measurement + MeasurementName + 'static>(c: &mut Criterion<M>) {
    let mut group = c.benchmark_group(format!("span_pool/{}", M::name()));
    for &(num_chunks, spans_per_chunk) in CONFIGS {
        let total = num_chunks * spans_per_chunk;
        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(
            format!("{num_chunks}x{spans_per_chunk}/allocating"),
            &(num_chunks, spans_per_chunk),
            |b, &(num_chunks, spans_per_chunk)| {
                b.iter(|| {
                    let chunks = build_chunks(
                        num_chunks,
                        spans_per_chunk,
                        Span::<BytesData>::default,
                        Vec::new,
                    );
                    // No pool: spans are dropped (deallocated) with no recycling.
                    drop(black_box(chunks));
                });
            },
        );
    }
    group.finish();
}

/// Multiple worker threads pull spans concurrently from a shared pool (contended dequeue via
/// [`SpanPool::get_span`]), each building one chunk, and hand the chunk to the main thread. The
/// main thread (single producer) collects every chunk and returns them all to the pool via
/// [`SpanPool::wrap_chunks`] + drop. Models the realistic producer/consumer pattern: many
/// tracer threads recycling spans through one shared pool while a single flush thread returns
/// them.
///
/// Unlike the single-threaded benches, the worker threads are **spawned and joined inside each
/// measured iteration**. That makes the timed region explicit (spawn → contended dequeue +
/// build → collect → single-producer enqueue → join) at the cost of including thread-spawn
/// overhead in the wall time. The batch is large ([`CONCURRENT_CONFIG`]) and throughput is
/// reported per span, so the per-span `thrpt` amortizes the spawn cost and isolates the pool
/// operations themselves.
fn concurrent_enqueue_dequeue_pooled<M: Measurement + MeasurementName + 'static>(
    c: &mut Criterion<M>,
) {
    let (num_chunks, spans_per_chunk) = CONCURRENT_CONFIG;
    let total = num_chunks * spans_per_chunk;
    let mut group = c.benchmark_group(format!("span_pool_concurrent/{}", M::name()));

    for &num_workers in NUM_WORKERS {
        let spans_per_worker = total / num_workers;
        let pool = SpanPool::<BytesData>::new(POOL_CAPACITY);
        warm_pool(&pool, total);

        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(
            format!("{num_workers}w/{num_chunks}x{spans_per_chunk}/pooled"),
            &(num_workers, spans_per_worker),
            |b, &(num_workers, spans_per_worker)| {
                b.iter(|| {
                    let (chunk_tx, chunk_rx) = mpsc::channel();

                    // Spawn workers: each pulls spans concurrently from the shared pool and
                    // builds one chunk.
                    let mut handles = Vec::with_capacity(num_workers);
                    for worker_idx in 0..num_workers {
                        let pool = pool.clone();
                        let chunk_tx = chunk_tx.clone();
                        let base = worker_idx as u64 * spans_per_worker as u64;
                        let trace_id = worker_idx as u128;
                        handles.push(thread::spawn(move || {
                            let chunk = build_chunk(
                                spans_per_worker,
                                base,
                                trace_id,
                                || pool.get_span(),
                                || pool.pull_empty_chunk(),
                            );
                            let _ = chunk_tx.send(chunk);
                        }));
                    }
                    // Drop the main's sender so `recv` ends once every worker has sent.
                    drop(chunk_tx);

                    // Block until every worker has finished building its chunk.
                    let chunks: Vec<Vec<Span<BytesData>>> = chunk_rx.iter().collect();

                    // Single producer: return every chunk to the pool at once.
                    drop(pool.wrap_chunks(black_box(chunks)));

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

/// Concurrent baseline: multiple worker threads allocate fresh spans (no pool), each building
/// one chunk, and hand it to the main thread. The main thread collects every chunk and drops it
/// (no recycling). Same shape as [`concurrent_enqueue_dequeue_pooled`] (spawn + join per
/// iteration, large batch, per-span throughput) but without the pool, so the comparison isolates
/// the pool's allocation savings and contention cost under concurrency.
fn concurrent_enqueue_dequeue_allocating<M: Measurement + MeasurementName + 'static>(
    c: &mut Criterion<M>,
) {
    let (num_chunks, spans_per_chunk) = CONCURRENT_CONFIG;
    let total = num_chunks * spans_per_chunk;
    let mut group = c.benchmark_group(format!("span_pool_concurrent/{}", M::name()));

    for &num_workers in NUM_WORKERS {
        let spans_per_worker = total / num_workers;

        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(
            format!("{num_workers}w/{num_chunks}x{spans_per_chunk}/allocating"),
            &(num_workers, spans_per_worker),
            |b, &(num_workers, spans_per_worker)| {
                b.iter(|| {
                    let (chunk_tx, chunk_rx) = mpsc::channel();

                    let mut handles = Vec::with_capacity(num_workers);
                    for worker_idx in 0..num_workers {
                        let chunk_tx = chunk_tx.clone();
                        let base = worker_idx as u64 * spans_per_worker as u64;
                        let trace_id = worker_idx as u128;
                        handles.push(thread::spawn(move || {
                            let chunk = build_chunk(
                                spans_per_worker,
                                base,
                                trace_id,
                                Span::<BytesData>::default,
                                Vec::new,
                            );
                            let _ = chunk_tx.send(chunk);
                        }));
                    }
                    drop(chunk_tx);

                    let chunks: Vec<Vec<Span<BytesData>>> = chunk_rx.iter().collect();
                    // No pool: chunks are dropped (deallocated) with no recycling.
                    drop(black_box(chunks));

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    span_pool_benches,
    enqueue_dequeue_pooled,
    enqueue_dequeue_allocating,
    concurrent_enqueue_dequeue_pooled,
    concurrent_enqueue_dequeue_allocating,
);
criterion_group!(
    name = span_pool_alloc_benches;
    config = memory_allocated_measurement(&super::GLOBAL);
    targets = enqueue_dequeue_pooled,  enqueue_dequeue_allocating,
        concurrent_enqueue_dequeue_pooled, concurrent_enqueue_dequeue_allocating,
);
