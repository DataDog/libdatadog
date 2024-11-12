// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, Criterion};
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use datadog_trace_utils::tracer_payload::{
    DefaultTraceChunkProcessor, TraceEncoding, TracerPayloadCollection, TracerPayloadParams,
};
use serde_json::{json, Value};

fn generate_spans(num_spans: usize, trace_id: u64) -> Vec<Value> {
    let mut spans = Vec::with_capacity(num_spans);
    let root_span_id = 100_000_000_000 + (trace_id % 1_000_000);

    for i in 0..num_spans {
        // If it's the first span make it the root
        let span_id = if i == 0 {
            root_span_id
        } else {
            root_span_id + i as u64 + 1
        };

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

pub fn deserialize_msgpack_to_internal(c: &mut Criterion) {
    // Generate roughly 10mb of data. This is the upper bound of payload size before a tracer
    // flushes
    let data = rmp_serde::to_vec(&generate_trace_chunks(20, 2_075))
        .expect("Failed to serialize test spans.");
    let data_as_bytes = tinybytes::Bytes::copy_from_slice(&data);
    let tracer_header_tags = &TracerHeaderTags::default();

    c.bench_function(
        "benching deserializing traces from msgpack to their internal representation ",
        |b| {
            b.iter_batched(
                || data_as_bytes.clone(),
                |data_as_bytes| {
                    let result: anyhow::Result<TracerPayloadCollection> = black_box(
                        TracerPayloadParams::new(
                            data_as_bytes,
                            tracer_header_tags,
                            &mut DefaultTraceChunkProcessor,
                            false,
                            TraceEncoding::V04,
                        )
                        .try_into(),
                    );
                    assert!(result.is_ok());
                    // Return the result to avoid measuring the deallocation time
                    result
                },
                criterion::BatchSize::LargeInput,
            );
        },
    );
}

criterion_group!(benches, deserialize_msgpack_to_internal);
