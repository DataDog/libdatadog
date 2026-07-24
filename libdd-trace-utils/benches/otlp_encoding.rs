// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Benchmarks for the OTLP encoder hot paths: mapping native spans to the prost OTLP IR, and
//! serializing that IR to the HTTP/protobuf and HTTP/JSON wire formats. Inputs are decoded from
//! msgpack into borrowed `SpanSlice`s, matching the production exporter path.

use criterion::{black_box, criterion_group, BatchSize, Criterion};
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::otlp_encoder::{
    encode_otlp_json, encode_otlp_protobuf, map_traces_to_otlp, OtlpResourceInfo,
};
use serde_json::{json, Value};

/// A realistic OTLP-bound span: a handful of string `meta` tags and a couple of numeric
/// `metrics`, so the per-span attribute work (the dominant cost) is exercised.
fn generate_spans(num_spans: usize, trace_id: u64) -> Vec<Value> {
    let root_span_id = 100_000_000_000 + (trace_id % 1_000_000);
    (0..num_spans)
        .map(|i| {
            let span_id = root_span_id + i as u64;
            let is_root = i == 0;
            let parent_id = if is_root { 0 } else { root_span_id };
            let mut meta = json!({
                "http.method": "GET",
                "http.url": "https://example.com/api/v1/users/12345",
                "http.status_code": "200",
                "env": "production",
                "version": "1.2.3",
                "component": "net/http",
            });
            if is_root {
                meta["_dd.p.tid"] = json!("5b8efff798038103");
            }
            json!({
                "service": "bench-service",
                "name": "http.request",
                "resource": "GET /api/v1/users",
                "trace_id": trace_id,
                "span_id": span_id,
                "parent_id": parent_id,
                "start": 1_544_712_660_000_000_000_i64 + i as i64,
                "duration": 1_000_000,
                "error": 0,
                "meta": meta,
                "metrics": { "_sampling_priority_v1": 1, "_dd.top_level": 1 },
                "type": "web",
            })
        })
        .collect()
}

fn generate_trace_chunks(num_chunks: usize, num_spans: usize) -> Vec<Vec<Value>> {
    (0..num_chunks)
        .map(|i| generate_spans(num_spans, 100_000_000_000 + i as u64))
        .collect()
}

fn resource_info() -> OtlpResourceInfo {
    // `OtlpResourceInfo` is `#[non_exhaustive]`, so build via Default + field assignment.
    let mut info = OtlpResourceInfo::default();
    info.service = "bench-service".to_string();
    info.env = "production".to_string();
    info.app_version = "1.2.3".to_string();
    info.language = "rust".to_string();
    info.tracer_version = "9.9.9".to_string();
    info.runtime_id = "11111111-2222-3333-4444-555555555555".to_string();
    info
}

pub fn otlp_encoding_benches(c: &mut Criterion) {
    let info = resource_info();

    // A single large trace of ~1000 spans gives a clean per-span signal. (A second "many small
    // traces" 100x10 fixture was dropped to keep the shared benchmark suite within its CI time
    // budget; both totalled ~1000 spans and gave similar signal.)
    let (num_chunks, num_spans) = (1usize, 1000usize);
    let id = format!("{num_chunks}x{num_spans}");
    let bytes = rmp_serde::to_vec(&generate_trace_chunks(num_chunks, num_spans))
        .expect("serialize fixture");
    let (spans, _) = msgpack_decoder::v04::from_slice(bytes.as_slice()).expect("decode fixture");

    // 1) native spans -> prost OTLP IR (the mapper).
    c.bench_function(&format!("otlp/map_to_prost/{id}"), |b| {
        b.iter_batched(
            || spans.clone(),
            |s| black_box(map_traces_to_otlp(black_box(s), &info, false)),
            BatchSize::SmallInput,
        )
    });

    // Pre-built IR for the encode-only benches (owned prost; no borrow of `bytes`).
    let req = map_traces_to_otlp(spans.clone(), &info, false);

    // 2) prost IR -> HTTP/protobuf bytes.
    c.bench_function(&format!("otlp/encode_protobuf/{id}"), |b| {
        b.iter(|| black_box(encode_otlp_protobuf(black_box(&req))))
    });

    // 3) prost IR -> OTLP/JSON bytes.
    c.bench_function(&format!("otlp/encode_json/{id}"), |b| {
        b.iter(|| black_box(encode_otlp_json(black_box(&req)).expect("json")))
    });

    // 4) end-to-end native spans -> protobuf wire (the real protobuf export path).
    c.bench_function(&format!("otlp/e2e_protobuf/{id}"), |b| {
        b.iter_batched(
            || spans.clone(),
            |s| {
                let req = map_traces_to_otlp(s, &info, false);
                black_box(encode_otlp_protobuf(&req))
            },
            BatchSize::SmallInput,
        )
    });

    // 5) end-to-end native spans -> JSON wire (the real JSON export path).
    c.bench_function(&format!("otlp/e2e_json/{id}"), |b| {
        b.iter_batched(
            || spans.clone(),
            |s| {
                let req = map_traces_to_otlp(s, &info, false);
                black_box(encode_otlp_json(&req).expect("json"))
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(otlp_benches, otlp_encoding_benches);
