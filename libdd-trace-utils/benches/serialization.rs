// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, Criterion};
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::msgpack_encoder;
use serde_json::{json, Value};

fn generate_spans(num_spans: usize, trace_id: u64) -> Vec<Value> {
    let mut spans = Vec::with_capacity(num_spans);
    let root_span_id = 100_000_000_000 + (trace_id % 1_000_000);

    for i in 0..num_spans {
        // If it's the first span make it the root
        let span_id = root_span_id + i as u64;

        // if it's not the root, then give it the root as a parent
        let parent_id = if i == 0 { 0 } else { root_span_id };

        spans.push(json!({
            "service": "test-service",
            "name": "test-service-name",
            "resource": "test-service-resource",
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_id": parent_id,
            "parent_id": 1,
            "start": 1,
            "duration": 5,
            "error": 0,
            "meta": {
                "app": "test-app",
                "thread.id": "58",
                "thread.name": "pool-5",
            },
            "metrics": {"_sampling_priority_v1": 2},
            "type": "http"
        }));
    }

    spans
}

fn generate_trace_chunks(num_chunks: usize, num_spans: usize) -> Vec<Vec<Value>> {
    let mut chunks = Vec::with_capacity(num_chunks);

    for i in 0..num_chunks {
        let trace_id = 100_000_000_000 + i as u64;
        let spans = generate_spans(num_spans, trace_id);
        chunks.push(spans);
    }

    chunks
}

pub fn serialize_internal_to_msgpack(c: &mut Criterion) {
    // Generate roughly 10mb of data. This is the upper bound of payload size before a tracer
    // flushes
    let data = rmp_serde::to_vec(&generate_trace_chunks(20, 2_075))
        .expect("Failed to serialize test spans.");
    let (data, ..) = msgpack_decoder::v04::from_slice(data.as_slice())
        .expect("Failed to deserialize test spans.");

    c.bench_function(
        "benching serializing traces from their internal representation to msgpack",
        |b| {
            b.iter_batched(
                || vec![0u8; 12_000_000],
                |mut vec| {
                    // rmp_serde
                    // let _ = black_box(rmp_serde::encode::write_named(&mut vec.as_mut_slice(),
                    // &data));
                    let _ = black_box(msgpack_encoder::v04::write_to_slice(
                        &mut vec.as_mut_slice(),
                        &data,
                    ));
                    // Return the result to avoid measuring the deallocation time
                    vec
                },
                criterion::BatchSize::LargeInput,
            );
        },
    );
}

criterion_group!(serialize_benches, serialize_internal_to_msgpack);
