// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
fn build_chunks<F: Fn() -> Span<BytesData>, G: Fn() -> Vec<Span<BytesData>>>(
    num_chunks: usize,
    spans_per_chunk: usize,
    get_span: F,
    pull_empty_chunk: G,
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

/// Warm the pool with `count` freshly-allocated, populated spans so that the first
/// measured iterations dequeue recycled spans (steady state) rather than allocating.
fn warm_pool(pool: &SpanPool<BytesData>, count: usize) {
    let chunks = build_chunks(1, count, Span::<BytesData>::default, Vec::new);
    // Returning the chunks to the pool recycles the spans (minus the ~10% dropped by the
    // drop policy).
    drop(pool.wrap_chunks(chunks));
}

fn bench_iter<
    M: Measurement,
    F: Fn() -> Span<BytesData>,
    G: Fn() -> Vec<Span<BytesData>>,
    H: Fn(Vec<Vec<Span<BytesData>>>),
>(
    group: &mut criterion::BenchmarkGroup<'_, M>,
    variant_name: &'static str,
    num_chunks: usize,
    spans_per_chunk: usize,
    get_span: F,
    get_chunk: G,
    return_chunks: H,
) {
    group.throughput(Throughput::Elements((num_chunks * spans_per_chunk) as u64));
    group.bench_with_input(
        format!("{num_chunks}x{spans_per_chunk}/{variant_name}"),
        &(num_chunks, spans_per_chunk),
        |b, &(num_chunks, spans_per_chunk)| {
            b.iter(|| {
                let chunks = build_chunks(num_chunks, spans_per_chunk, &get_span, &get_chunk);
                // Enqueue: returning the chunks to the pool recycles the spans.
                black_box(return_chunks(black_box(chunks)));
            });
        },
    );
}

/// Dequeue spans from the pool, populate them, build chunks, then enqueue the chunks back
/// into the pool (recycling the spans on drop). Throughput is reported per span.
fn enqueue_dequeue<M: Measurement + MeasurementName + 'static>(c: &mut Criterion<M>) {
    let mut group = c.benchmark_group(format!("span_pool/{}", M::name()));
    for &(num_chunks, spans_per_chunk) in CONFIGS {
        let total = num_chunks * spans_per_chunk;
        let pool = SpanPool::<BytesData>::new(POOL_CAPACITY);
        warm_pool(&pool, total);

        bench_iter(
            &mut group,
            "pooled",
            num_chunks,
            spans_per_chunk,
            || pool.get_span(),
            || pool.pull_empty_chunk(),
            |chunks| drop(pool.wrap_chunks(chunks)),
        );

        bench_iter(
            &mut group,
            "allocating",
            num_chunks,
            spans_per_chunk,
            Span::<BytesData>::default,
            Vec::new,
            drop,
        );
    }
    group.finish();
}

criterion_group!(span_pool_benches, enqueue_dequeue,);
criterion_group!(
    name = span_pool_alloc_benches;
    config = memory_allocated_measurement(&super::GLOBAL);
    targets = enqueue_dequeue,
);
