// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Splitting a batch several times the payload limit, compared with one unbounded encoding of the
//! whole batch. Packing should cost about as much as encoding.

use criterion::{Criterion, black_box, criterion_group};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::msgpack_encoder::{PushError, v1, v04, v05};
use libdd_trace_utils::span::v04::SpanBytes;
use libdd_trace_utils::trace_utils::convert_trace_chunks_v04_to_v05;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use libdd_trace_utils::tracer_payload::TraceChunks;

const LIMIT: usize = 1024 * 1024;

fn span(id: u64) -> SpanBytes {
    let text = |s: String| BytesString::from(s);
    let mut span = SpanBytes {
        service: text("web-store".into()),
        name: text("rack.request".into()),
        resource: text(format!("GET /api/v2/products/{id:>08}/reviews")),
        r#type: text("web".into()),
        trace_id: u128::from(id / 10),
        span_id: id,
        parent_id: id.saturating_sub(1),
        start: 1_700_000_000_000_000_000,
        duration: 1_234_567,
        ..Default::default()
    };
    for (key, value) in [
        ("http.method", "GET".to_owned()),
        (
            "http.url",
            format!("https://example.com/api/v2/products/{id}"),
        ),
        ("http.status_code", "200".into()),
        ("component", "rack".into()),
        ("span.kind", "server".into()),
        ("http.route", "/api/v2/products/:id/reviews".into()),
    ] {
        span.meta.insert(text(key.into()), text(value));
    }
    span.metrics
        .insert(text("_sampling_priority_v1".into()), 1.0);
    span
}

/// Ten-span chunks adding up to a few times `LIMIT`, depending on the format.
fn chunks() -> Vec<Vec<SpanBytes>> {
    (0..1_000u64)
        .map(|chunk| (0..10).map(|index| span(chunk * 10 + index)).collect())
        .collect()
}

/// The greedy loop a consumer runs: oversized chunks are skipped without closing the payload.
macro_rules! pack {
    ($chunks:expr, $new:expr) => {{
        let mut payloads = Vec::new();
        let mut packer = $new;
        for chunk in $chunks {
            if let Err(PushError::Full) = packer.push(chunk) {
                let mut next = $new;
                if next.push(chunk).is_ok() {
                    payloads.push(std::mem::replace(&mut packer, next).finish());
                }
            }
        }
        payloads.push(packer.finish());
        payloads
    }};
}

fn chunk_packing(c: &mut Criterion) {
    let chunks = chunks();
    let metadata = TracerMetadata::default();
    let mut group = c.benchmark_group("chunk_packing");

    group.bench_function("v04/encode", |b| {
        b.iter(|| black_box(v04::to_vec_from_v04(&chunks)))
    });
    group.bench_function("v04/pack", |b| {
        b.iter(|| black_box(pack!(&chunks, v04::ChunkPacker::new(LIMIT))))
    });
    group.bench_function("v1/encode", |b| {
        b.iter(|| black_box(v1::to_vec_from_v04(&chunks, &metadata)))
    });
    group.bench_function("v1/pack", |b| {
        b.iter(|| black_box(pack!(&chunks, v1::ChunkPacker::new(&metadata, LIMIT))))
    });
    group.bench_function("v05/encode", |b| {
        b.iter(|| {
            let Ok(TraceChunks::V05(payload)) = convert_trace_chunks_v04_to_v05(&chunks) else {
                unreachable!()
            };
            black_box(rmp_serde::to_vec(&payload))
        })
    });
    group.bench_function("v05/pack", |b| {
        b.iter(|| black_box(pack!(&chunks, v05::ChunkPacker::new(LIMIT))))
    });
    group.finish();
}

criterion_group!(chunk_packing_benches, chunk_packing);
