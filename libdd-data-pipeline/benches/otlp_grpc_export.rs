// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Benchmarks for the OTLP gRPC export hot path.
//!
//! The gRPC exporter turns native trace chunks into the length-prefixed gRPC
//! wire frame that tonic's codec puts on the socket once per export. The prost
//! protobuf encoding is shared with the HTTP/protobuf path (already covered by
//! `libdd-trace-utils/benches/otlp_encoding.rs`); these benches measure the
//! gRPC-specific framing on top of it, plus the full native-spans -> wire-frame
//! preparation across trace sizes so the per-span cost is visible.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
use prost::Message;
use serde_json::{json, Value};

/// A realistic OTLP-bound span: a handful of string `meta` tags and a couple of
/// numeric `metrics`, so the per-span attribute work (the dominant cost) is
/// exercised. Mirrors the fixture in `libdd-trace-utils/benches/otlp_encoding.rs`.
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

/// Frame protobuf bytes exactly as gRPC does: a 1-byte compression flag and a
/// 4-byte big-endian length prefix, then the message body.
fn grpc_frame(body: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(5 + body.len());
    framed.push(0u8); // compression flag: 0 = uncompressed
    framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
    framed.extend_from_slice(body);
    framed
}

pub fn grpc_export_benches(c: &mut Criterion) {
    let info = resource_info();

    for &num_spans in &[1usize, 1000usize] {
        let id = format!("1x{num_spans}");
        let bytes =
            rmp_serde::to_vec(&vec![generate_spans(num_spans, 100_000_000_000)]).expect("fixture");
        let (spans, _) =
            msgpack_decoder::v04::from_slice(bytes.as_slice()).expect("decode fixture");
        let req = map_traces_to_otlp(spans.clone(), &info, false);

        // Encode-only: prost OTLP IR -> gRPC wire frame (what the codec emits per export).
        c.bench_function(&format!("grpc/encode_framed/{id}"), |b| {
            b.iter(|| {
                let body = black_box(&req).encode_to_vec();
                black_box(grpc_frame(&body))
            })
        });

        // End-to-end: native spans -> mapped OTLP IR -> gRPC wire frame.
        c.bench_function(&format!("grpc/e2e_framed/{id}"), |b| {
            b.iter_batched(
                || spans.clone(),
                |s| {
                    let req = map_traces_to_otlp(s, &info, false);
                    let body = req.encode_to_vec();
                    black_box(grpc_frame(&body))
                },
                BatchSize::SmallInput,
            )
        });
    }
}

criterion_group!(benches, grpc_export_benches);
criterion_main!(benches);
