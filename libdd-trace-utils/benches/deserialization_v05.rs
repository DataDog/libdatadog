// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Microbenchmarks for the V05 msgpack decode path
//! (`libdd_trace_utils::msgpack_decoder::v05`).
//!
//! Unlike V04, the V05 wire format encodes a shared string dictionary up front and every span
//! references its string fields (service, name, resource, type, and every meta/metrics key/value)
//! by index into that dictionary. Decoding therefore performs a dictionary lookup-and-clone for
//! each string slot, which is real per-span work on the agent ingestion hot path and is otherwise
//! uncovered by benchmarks.
//!
//! We vary two axes that drive that cost:
//!   * the number of spans (work scales with span count), and
//!   * the string-sharing ratio: a "high sharing" payload reuses a small dictionary across all
//!     spans (the common case for tracer traffic, where service/name/tag keys repeat), while a "low
//!     sharing" payload gives each span its own unique strings (a larger dictionary and worse cache
//!     behavior).

use std::alloc::System;
use std::collections::HashMap;

use criterion::{black_box, criterion_group, BenchmarkId, Criterion, Throughput};
use libdd_common::bench_utils::{memory_allocated_measurement, AllocatedBytesMeasurement};
use libdd_trace_utils::msgpack_decoder;

/// A V05 span is a fixed 12-element tuple. String fields (service, name, resource, type, and the
/// meta/metrics keys/values) are `u32` indices into the shared dictionary.
type V05Span = (
    u32,               // service (dict index)
    u32,               // name (dict index)
    u32,               // resource (dict index)
    u64,               // trace_id
    u64,               // span_id
    u64,               // parent_id
    i64,               // start
    i64,               // duration
    i32,               // error
    HashMap<u32, u32>, // meta (dict index -> dict index)
    HashMap<u32, f64>, // metrics (dict index -> value)
    u32,               // type (dict index)
);

type V05Payload = (Vec<String>, Vec<Vec<V05Span>>);

/// Number of meta tags per span. Picked to resemble a typical instrumented span (service/runtime
/// metadata, a couple of resource attributes, thread info, etc.).
const META_TAGS_PER_SPAN: usize = 8;
/// Number of metrics per span (sampling priority, a couple of measured values).
const METRICS_PER_SPAN: usize = 3;

/// Builds a representative V05 payload.
///
/// `unique_per_span` controls the string-sharing ratio:
///   * `false` (high sharing): all spans draw their strings from a single small shared dictionary,
///     mirroring real tracer traffic where service names and tag keys repeat heavily.
///   * `true` (low sharing): each span contributes its own unique strings, producing a large
///     dictionary with little reuse.
///
/// Data is fully deterministic.
fn build_v05_payload(num_traces: usize, spans_per_trace: usize, unique_per_span: bool) -> Vec<u8> {
    let mut dict: Vec<String> = Vec::new();
    let intern = |s: String, dict: &mut Vec<String>| -> u32 {
        let idx = dict.len() as u32;
        dict.push(s);
        idx
    };

    // Shared dictionary entries reused by every span in the "high sharing" scenario.
    let shared_service = intern("test-service".to_string(), &mut dict);
    let shared_name = intern("test-service.handler".to_string(), &mut dict);
    let shared_resource = intern("GET /api/v1/resource".to_string(), &mut dict);
    let shared_type = intern("web".to_string(), &mut dict);

    // Shared meta keys/values (typical tag keys that repeat across all spans).
    let shared_meta: Vec<(u32, u32)> = [
        ("env", "production"),
        ("version", "1.2.3"),
        ("runtime-id", "f0e1d2c3-b4a5-6789-0abc-def012345678"),
        ("language", "rust"),
        ("component", "http"),
        ("span.kind", "server"),
        ("http.method", "GET"),
        ("http.status_code", "200"),
    ]
    .iter()
    .take(META_TAGS_PER_SPAN)
    .map(|(k, v)| {
        (
            intern((*k).to_string(), &mut dict),
            intern((*v).to_string(), &mut dict),
        )
    })
    .collect();

    let shared_metric_keys: Vec<u32> = ["_sampling_priority_v1", "_dd.measured", "_dd.top_level"]
        .iter()
        .take(METRICS_PER_SPAN)
        .map(|k| intern((*k).to_string(), &mut dict))
        .collect();

    let mut traces: Vec<Vec<V05Span>> = Vec::with_capacity(num_traces);

    for trace_idx in 0..num_traces {
        let mut spans: Vec<V05Span> = Vec::with_capacity(spans_per_trace);
        let root_span_id = 100_000_000_000u64 + trace_idx as u64;

        for span_idx in 0..spans_per_trace {
            let span_id = root_span_id + span_idx as u64 + 1;
            let parent_id = if span_idx == 0 { 0 } else { root_span_id };

            let (service, name, resource, ty, meta, metric_keys) = if unique_per_span {
                // Low sharing: every span interns its own unique strings.
                let service = intern(format!("service-{trace_idx}-{span_idx}"), &mut dict);
                let name = intern(format!("op-{trace_idx}-{span_idx}"), &mut dict);
                let resource = intern(format!("GET /api/{trace_idx}/{span_idx}"), &mut dict);
                let ty = intern(format!("type-{}", span_idx % 4), &mut dict);

                let meta: Vec<(u32, u32)> = (0..META_TAGS_PER_SPAN)
                    .map(|m| {
                        (
                            intern(format!("tag.key.{trace_idx}.{span_idx}.{m}"), &mut dict),
                            intern(format!("tag-value-{trace_idx}-{span_idx}-{m}"), &mut dict),
                        )
                    })
                    .collect();
                let metric_keys: Vec<u32> = (0..METRICS_PER_SPAN)
                    .map(|m| intern(format!("metric.{trace_idx}.{span_idx}.{m}"), &mut dict))
                    .collect();

                (service, name, resource, ty, meta, metric_keys)
            } else {
                // High sharing: reuse the shared dictionary entries.
                (
                    shared_service,
                    shared_name,
                    shared_resource,
                    shared_type,
                    shared_meta.clone(),
                    shared_metric_keys.clone(),
                )
            };

            let meta_map: HashMap<u32, u32> = meta.into_iter().collect();
            let metrics_map: HashMap<u32, f64> = metric_keys
                .into_iter()
                .enumerate()
                .map(|(i, k)| (k, i as f64 + 1.0))
                .collect();

            spans.push((
                service,
                name,
                resource,
                100_000_000_000u64 + trace_idx as u64,
                span_id,
                parent_id,
                1_700_000_000_000_000_000i64,
                123_456i64,
                0i32,
                meta_map,
                metrics_map,
                ty,
            ));
        }
        traces.push(spans);
    }

    let payload: V05Payload = (dict, traces);
    rmp_serde::to_vec(&payload).expect("Failed to serialize V05 test payload.")
}

/// Runs the V05 decode benchmark matrix against the given criterion harness, which lets us reuse
/// the same scenarios for both the wall-time and the bytes-allocated measurements.
///
/// Note: `Throughput::Bytes` is reported per scenario so each series can be read as a decode rate,
/// but high-sharing and low-sharing payloads have very different encoded sizes for the same span
/// count, so their throughput numbers are *not* directly comparable across the sharing dimension.
fn bench_v05_matrix<M: criterion::measurement::Measurement>(
    c: &mut Criterion<M>,
    group_name: &str,
) {
    // A representative chunked payload: 20 traces (the upper-bound trace count for a tracer flush)
    // with a varying number of spans each. Span counts stay realistic for a single flush while
    // exercising the dictionary-dedup path across many spans.
    const NUM_TRACES: usize = 20;
    let span_counts = [10usize, 100, 500];

    let mut group = c.benchmark_group(group_name);

    for &spans_per_trace in &span_counts {
        for (sharing_label, unique_per_span) in [("high_sharing", false), ("low_sharing", true)] {
            let data = build_v05_payload(NUM_TRACES, spans_per_trace, unique_per_span);
            let data_as_bytes = libdd_tinybytes::Bytes::copy_from_slice(&data);

            group.throughput(Throughput::Bytes(data.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(sharing_label, spans_per_trace * NUM_TRACES),
                &data_as_bytes,
                |b, data_as_bytes| {
                    b.iter_batched(
                        || data_as_bytes.clone(),
                        |data_as_bytes| {
                            let result = black_box(msgpack_decoder::v05::from_bytes(data_as_bytes));
                            assert!(result.is_ok());
                            // Return the result to avoid measuring the deallocation time.
                            result
                        },
                        criterion::BatchSize::LargeInput,
                    );
                },
            );
        }
    }

    group.finish();
}

fn deserialize_msgpack_v05(c: &mut Criterion) {
    bench_v05_matrix(c, "msgpack_decoder::v05");
}

/// Allocation-measured counterpart. The dictionary-dedup path clones a `Bytes` slice for every
/// string slot, so the amount allocated per decode is the metric most directly affected by the
/// sharing ratio.
fn deserialize_msgpack_v05_allocs(c: &mut Criterion<AllocatedBytesMeasurement<System>>) {
    bench_v05_matrix(c, "msgpack_decoder::v05 (allocs)");
}

criterion_group!(deserialize_v05_benches, deserialize_msgpack_v05);
criterion_group!(
    name = deserialize_v05_alloc_benches;
    config = memory_allocated_measurement(&super::GLOBAL);
    targets = deserialize_msgpack_v05_allocs
);
