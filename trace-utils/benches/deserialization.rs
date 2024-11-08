// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, Criterion};
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use datadog_trace_utils::tracer_payload::{
    DefaultTraceChunkProcessor, TraceEncoding, TracerPayloadCollection, TracerPayloadParams,
};
use serde_json::json;

pub fn deserialize_msgpack_to_internal(c: &mut Criterion) {
    let span_data1 = json!([{
        "service": "test-service",
        "name": "test-service-name",
        "resource": "test-service-resource",
        "trace_id": 111,
        "span_id": 222,
        "parent_id": 100,
        "start": 1,
        "duration": 5,
        "error": 0,
        "meta": {},
        "metrics": {},
    }]);

    let span_data2 = json!([{
        "service": "test-service",
        "name": "test-service-name",
        "resource": "test-service-resource",
        "trace_id": 111,
        "span_id": 333,
        "parent_id": 100,
        "start": 1,
        "duration": 5,
        "error": 1,
        "meta": {},
        "metrics": {},
    }]);

    let data =
        rmp_serde::to_vec(&vec![span_data1, span_data2]).expect("Failed to serialize test spans.");
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
