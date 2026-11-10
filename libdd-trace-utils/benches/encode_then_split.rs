// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{BenchmarkId, Criterion, criterion_group};
use libdd_trace_utils::{
    msgpack_encoder::{
        split::{self, Format},
        v1, v04,
    },
    span::v1::{SpanBytes, TraceChunkBytes, TracerPayloadBytes},
};
use std::{hint::black_box, time::Duration};

fn payload(chunks: u64, spans: u64) -> TracerPayloadBytes {
    TracerPayloadBytes {
        env: "production".into(),
        chunks: (0..chunks)
            .map(|i| TraceChunkBytes {
                trace_id: u128::from(i + 1).to_be_bytes(),
                priority: Some(1),
                spans: (0..spans)
                    .map(|j| SpanBytes {
                        service: "web".into(),
                        name: "request".into(),
                        resource: format!("{i}/{j}/{}", "x".repeat(512)).into(),
                        span_id: j + 1,
                        parent_id: u64::from(j != 0),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn encode(source: &TracerPayloadBytes, format: Format) -> Vec<u8> {
    match format {
        Format::V04 => v04::to_vec_from_v1(source),
        Format::V1 => v1::to_vec_from_v1(source),
    }
}

fn bench(c: &mut Criterion) {
    let ordinary = payload(32, 20);
    let singleton = payload(1, 640);
    let mut large_span = payload(1, 20);
    large_span.chunks[0].spans[10].resource = "x".repeat(1024 * 1024).into();
    let mut group = c.benchmark_group("encode_then_split");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for format in [Format::V04, Format::V1] {
        let name = format!("{format:?}");
        group.bench_function(BenchmarkId::new(&name, "encode_once"), |b| {
            b.iter(|| black_box(encode(black_box(&ordinary), format)));
        });
        for (case, source, limit) in [
            ("fits", &ordinary, encode(&ordinary, format).len()),
            ("chunks", &ordinary, 64 * 1024),
            ("spans", &singleton, 64 * 1024),
            ("large_span", &large_span, 64 * 1024),
        ] {
            group.bench_function(BenchmarkId::new(&name, case), |b| {
                b.iter(|| {
                    for outcome in split::encode(black_box(source), format, limit) {
                        black_box(outcome);
                    }
                });
            });
        }
    }
    group.finish();
}

criterion_group!(encode_then_split_benches, bench);
