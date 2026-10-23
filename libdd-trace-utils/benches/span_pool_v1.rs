// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::measurement::Measurement;
use criterion::{Criterion, Throughput, black_box, criterion_group};
use libdd_common::bench_utils::{MeasurementName, memory_allocated_criterion};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::BytesData;
use libdd_trace_utils::span::v1::span_pool::SpanPoolV1;
use libdd_trace_utils::span::v1::{AttributeValue, Span, TraceChunk};

/// Configurations exercised by the benchmarks: `(number of chunks, spans per chunk)`.
/// The small one resembles a single trace flush; the large one resembles a full payload.
const CONFIGS: &[(usize, usize)] = &[(1, 10), (20, 100)];

/// Pool capacity: large enough to hold a full iteration's worth of recycled spans and chunks, so
/// the bounded channels never drop items because they're full (only the drop policy does).
const POOL_CAPACITY: usize = 4_096;

/// Populate a span with realistic fields and a few attributes.
///
/// Inserting into `attributes` is where the pool pays off: a recycled span keeps its `VecMap`'s
/// backing `Vec` capacity, so the inserts reuse it instead of growing a fresh allocation from
/// zero.
fn populate_span(span: &mut Span<BytesData>, span_idx: u64) {
    span.service = BytesString::from_static("test-service");
    span.name = BytesString::from_static("http.request");
    span.resource = BytesString::from_static("GET /api/resource");
    span.r#type = BytesString::from_static("http");
    span.span_id = span_idx;
    span.parent_id = if span_idx == 0 { 0 } else { span_idx - 1 };
    span.start = 1_000_000 + span_idx as i64;
    span.duration = 5_000;
    span.error = false;
    span.attributes.insert(
        BytesString::from_static("http.method"),
        AttributeValue::String(BytesString::from_static("GET")),
    );
    span.attributes.insert(
        BytesString::from_static("http.route"),
        AttributeValue::String(BytesString::from_static("/api/echo")),
    );
    span.attributes.insert(
        BytesString::from_static("http.status_code"),
        AttributeValue::Int(200),
    );
    span.attributes.insert(
        BytesString::from_static("language"),
        AttributeValue::String(BytesString::from_static("python")),
    );
    span.attributes.insert(
        BytesString::from_static("_sampling_priority_v1"),
        AttributeValue::Int(2),
    );
    span.attributes.insert(
        BytesString::from_static("_dd.top_level"),
        AttributeValue::Bool(true),
    );
}

/// Populate a chunk's own (span-less) fields and a couple of chunk-level attributes.
fn populate_chunk(chunk: &mut TraceChunk<BytesData>, chunk_idx: usize) {
    chunk.trace_id = (chunk_idx as u128).to_be_bytes();
    chunk.priority = Some(1);
    chunk.origin = BytesString::from_static("rust");
    chunk.attributes.insert(
        BytesString::from_static("_dd.p.dm"),
        AttributeValue::String(BytesString::from_static("-0")),
    );
}

/// Build `num_chunks` chunks of `spans_per_chunk` spans each, via `get_chunk`/`get_span`, which
/// decide whether chunks/spans come from the pool or are freshly allocated.
fn build_chunks<F: Fn() -> Span<BytesData>, G: Fn() -> TraceChunk<BytesData>>(
    num_chunks: usize,
    spans_per_chunk: usize,
    get_span: F,
    get_chunk: G,
) -> Vec<TraceChunk<BytesData>> {
    let mut chunks = Vec::with_capacity(num_chunks);
    for chunk_idx in 0..num_chunks {
        let mut chunk = get_chunk();
        populate_chunk(&mut chunk, chunk_idx);
        chunk.spans.reserve(spans_per_chunk);
        let base = chunk_idx * spans_per_chunk;
        for i in 0..spans_per_chunk {
            let mut span = get_span();
            populate_span(&mut span, base as u64 + i as u64);
            chunk.spans.push(span);
        }
        chunks.push(chunk);
    }
    chunks
}

/// Warm the pool with `count` freshly-allocated, populated spans (in a single chunk) so that the
/// first measured iterations dequeue recycled spans/chunks (steady state) rather than allocating.
fn warm_pool(pool: &SpanPoolV1<BytesData>, count: usize) {
    let chunks = build_chunks(1, count, Span::default, TraceChunk::default);
    // Returning the chunks to the pool recycles both the spans and the chunk shells (minus the
    // ~10% dropped by each sub-pool's drop policy).
    drop(pool.wrap_chunks(chunks));
}

#[allow(clippy::too_many_arguments)]
fn bench_iter<
    M: Measurement,
    F: Fn() -> Span<BytesData>,
    G: Fn() -> TraceChunk<BytesData>,
    H: Fn(Vec<TraceChunk<BytesData>>),
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
                // Enqueue: returning the chunks to the pool recycles the spans and chunk shells.
                return_chunks(black_box(chunks));
            });
        },
    );
}

/// Dequeue chunks/spans from the pool, populate them, then enqueue the chunks back into the pool
/// (recycling spans and chunk shells on drop). Throughput is reported per span.
fn enqueue_dequeue<M: Measurement + MeasurementName + 'static>(c: &mut Criterion<M>) {
    let mut group = c.benchmark_group(format!("span_pool_v1/{}", M::name()));
    for &(num_chunks, spans_per_chunk) in CONFIGS {
        let total = num_chunks * spans_per_chunk;
        let pool = SpanPoolV1::<BytesData>::with_capacity(POOL_CAPACITY);
        warm_pool(&pool, total);

        bench_iter(
            &mut group,
            "pooled",
            num_chunks,
            spans_per_chunk,
            || pool.get_span(),
            || pool.get_chunk(),
            |chunks| drop(pool.wrap_chunks(chunks)),
        );

        bench_iter(
            &mut group,
            "allocating",
            num_chunks,
            spans_per_chunk,
            Span::<BytesData>::default,
            TraceChunk::<BytesData>::default,
            drop,
        );
    }
    group.finish();
}

criterion_group!(span_pool_v1_benches, enqueue_dequeue,);
// Not `criterion_group!`: its `config =` would be overridden by the command-line flags.
pub fn span_pool_v1_alloc_benches() {
    let mut criterion = memory_allocated_criterion(&super::GLOBAL);
    enqueue_dequeue(&mut criterion);
}
