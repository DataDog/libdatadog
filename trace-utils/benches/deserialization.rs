// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::Bytes;
use criterion::{black_box, criterion_group, Criterion};
use datadog_trace_utils::no_alloc_span::Span;
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use datadog_trace_utils::tracer_payload::{
    msgpack_to_tracer_payload_collection_no_alloc, msgpack_to_tracer_payload_collection, TraceEncoding,
    TracerPayloadCollection, TracerPayloadParams,
};
use rmp_serde::to_vec;
use serde_json::json;
use rmp_serde::Deserializer;
use datadog_trace_protobuf::pb;

fn create_span_data() -> Vec<u8> {
    let mut spans = vec![];

    for i in 0..100 {
        let span_data1 = json!([{
            "service": "test-service",
            "name": "test-service-name",
            "resource": "test-service-resource",
            "trace_id": 111 + i,
            "span_id": 222 + i,
            "parent_id": 100 + i,
            "start": 1 + i,
            "duration": 5 + i,
            "error": 0,
            "type": "web",
            "meta": {
                "user": "test-user",
                "env": "production",
                "version": "1.0.0",
                "region": "us-east-1",
                "role": "backend"
            },
            "metrics": {
                "cpu_usage": 0.75 + (i as f64) * 0.01,
                "memory_usage": 128 + i,
                "disk_io": 100 + i,
                "network_io": 200 + i,
                "uptime": 3600 + i
            },
            "span_links": [
                {
                    "trace_id": 222 + i,
                    "trace_id_high": 0,
                    "span_id": 444 + i,
                    "attributes": {
                        "key1": "value1",
                        "key2": "value2"
                    },
                    "tracestate": "state1",
                    "flags": 1
                },
                {
                    "trace_id": 333 + i,
                    "trace_id_high": 0,
                    "span_id": 555 + i,
                    "attributes": {
                        "key3": "value3",
                        "key4": "value4"
                    },
                    "tracestate": "state2",
                    "flags": 1
                },
                {
                    "trace_id": 444 + i,
                    "trace_id_high": 0,
                    "span_id": 666 + i,
                    "attributes": {
                        "key5": "value5",
                        "key6": "value6"
                    },
                    "tracestate": "state3",
                    "flags": 1
                }
            ]
        }]);

        spans.push(span_data1);
    }
    to_vec(&spans).expect("Failed to serialize test spans.")
}

pub fn deserialize_msgpack_to_internal(c: &mut Criterion) {
    let data = create_span_data();
    let tracer_header_tags = &TracerHeaderTags::default();

    c.bench_function(
        "benching deserializing traces from msgpack to their internal representation ",
        |b| {
            b.iter_batched(
                || &data,
                |data| {
                    let result: anyhow::Result<Vec<Vec<pb::Span>>> =
                        black_box(msgpack_to_tracer_payload_collection(data));
                    assert!(result.is_ok());
                    // Return the result to avoid measuring the deallocation time
                    result
                },
                criterion::BatchSize::LargeInput,
            );
        },
    );
}

pub fn deserialize_msgpack_to_internal_no_alloc(c: &mut Criterion) {
    let vec_data = create_span_data();
    let data = Bytes::from(vec_data);

    c.bench_function(
        "benching deserializing traces from msgpack to their internal no-alloc representation ",
        |b| {
            b.iter_batched(
                || data.clone(),
                |data| {
                    let result: anyhow::Result<Vec<Vec<Span>>> =
                        black_box(msgpack_to_tracer_payload_collection_no_alloc(data));
                    assert!(result.is_ok());
                    // Return the result to avoid measuring the deallocation time
                    result
                },
                criterion::BatchSize::LargeInput,
            );
        },
    );
}

pub fn deserialize_msgpack_serde_to_pb(c: &mut Criterion) {
    let data = create_span_data();

    c.bench_function(
        "benching deserializing traces from msgpack using serde to protobuf",
        |b| {
            b.iter_batched(
                || &data,
                |data| {
                    let result: Result<Vec<Vec<pb::Span>>, _>= black_box(rmp_serde::from_slice(data));
                    assert!(result.is_ok());
                    // Return the result to avoid measuring the deallocation time
                    result
                },
                criterion::BatchSize::LargeInput,
            );
        },
    );

}

criterion_group!(
    benches,
    deserialize_msgpack_to_internal,
    deserialize_msgpack_to_internal_no_alloc,
    deserialize_msgpack_serde_to_pb
);
