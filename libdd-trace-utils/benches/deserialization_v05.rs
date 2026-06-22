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

use criterion::{black_box, criterion_group, BenchmarkId, Criterion, Throughput};
use libdd_common::bench_utils::{memory_allocated_measurement, AllocatedBytesMeasurement};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::span::v04::SpanBytes;
use libdd_trace_utils::span::v05::{self, from_v04_span};
use libdd_trace_utils::span::vec_map::VecMap;
use libdd_trace_utils::span::SharedDictBytes;

/// Number of meta tags per span. Picked to resemble a typical instrumented span (service/runtime
/// metadata, a couple of resource attributes, thread info, etc.).
const META_TAGS_PER_SPAN: usize = 8;
/// Number of metrics per span (sampling priority, a couple of measured values).
const METRICS_PER_SPAN: usize = 3;

/// Builds a representative V05 payload.
///
/// The payload is assembled as V04 spans and then run through the production `from_v04_span`
/// interning path (the same logic `collect_trace_chunks` uses for `TraceEncoding::V05`), so the
/// shared dictionary and the wire layout match what the encoder actually emits.
///
/// `unique_per_span` controls the string-sharing ratio:
///   * `false` (high sharing): all spans share the same string values, which `from_v04_span`
///     collapses into a single small dictionary, mirroring real tracer traffic where service names
///     and tag keys repeat heavily.
///   * `true` (low sharing): each span contributes its own unique strings, producing a large
///     dictionary with little reuse.
///
/// Data is fully deterministic.
fn build_v05_payload(num_traces: usize, spans_per_trace: usize, unique_per_span: bool) -> Vec<u8> {
    // Shared string values reused by every span in the "high sharing" scenario.
    const SHARED_META: [(&str, &str); META_TAGS_PER_SPAN] = [
        ("env", "production"),
        ("version", "1.2.3"),
        ("runtime-id", "f0e1d2c3-b4a5-6789-0abc-def012345678"),
        ("language", "rust"),
        ("component", "http"),
        ("span.kind", "server"),
        ("http.method", "GET"),
        ("http.status_code", "200"),
    ];
    const SHARED_METRIC_KEYS: [&str; METRICS_PER_SPAN] =
        ["_sampling_priority_v1", "_dd.measured", "_dd.top_level"];

    let mut traces = Vec::with_capacity(num_traces);

    for trace_idx in 0..num_traces {
        let mut spans = Vec::with_capacity(spans_per_trace);
        let root_span_id = 100_000_000_000 + trace_idx as u64;

        for span_idx in 0..spans_per_trace {
            let span_id = root_span_id + span_idx as u64 + 1;
            let parent_id = if span_idx == 0 { 0 } else { root_span_id };

            let (service, name, resource, ty, meta, metrics) = if unique_per_span {
                // Low sharing: every span contributes its own unique strings.
                let meta: VecMap<BytesString, BytesString> = (0..META_TAGS_PER_SPAN)
                    .map(|m| {
                        (
                            BytesString::from(format!("tag.key.{trace_idx}.{span_idx}.{m}")),
                            BytesString::from(format!("tag-value-{trace_idx}-{span_idx}-{m}")),
                        )
                    })
                    .collect();
                let metrics: VecMap<BytesString, f64> = (0..METRICS_PER_SPAN)
                    .map(|m| {
                        (
                            BytesString::from(format!("metric.{trace_idx}.{span_idx}.{m}")),
                            m as f64 + 1.0,
                        )
                    })
                    .collect();

                (
                    BytesString::from(format!("service-{trace_idx}-{span_idx}")),
                    BytesString::from(format!("op-{trace_idx}-{span_idx}")),
                    BytesString::from(format!("GET /api/{trace_idx}/{span_idx}")),
                    BytesString::from(format!("type-{}", span_idx % 4)),
                    meta,
                    metrics,
                )
            } else {
                // High sharing: reuse the shared string values (typical tag keys that repeat
                // across all spans).
                let meta: VecMap<BytesString, BytesString> = SHARED_META
                    .iter()
                    .map(|(k, v)| (BytesString::from(*k), BytesString::from(*v)))
                    .collect();
                let metrics: VecMap<BytesString, f64> = SHARED_METRIC_KEYS
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (BytesString::from(*k), i as f64 + 1.0))
                    .collect();

                (
                    BytesString::from("test-service"),
                    BytesString::from("test-service.handler"),
                    BytesString::from("GET /api/v1/resource"),
                    BytesString::from("web"),
                    meta,
                    metrics,
                )
            };

            spans.push(SpanBytes {
                service,
                name,
                resource,
                r#type: ty,
                trace_id: 100_000_000_000u128 + trace_idx as u128,
                span_id,
                parent_id,
                start: 1_700_000_000_000_000_000,
                duration: 123_456,
                error: 0,
                meta,
                metrics,
                meta_struct: VecMap::default(),
                span_links: Vec::new(),
                span_events: Vec::new(),
            });
        }
        traces.push(spans);
    }

    // Intern every string into the shared dictionary via the production V05 conversion path.
    let mut dict = SharedDictBytes::default();
    let v05_traces: Vec<Vec<v05::Span>> = traces
        .into_iter()
        .map(|trace| {
            trace
                .into_iter()
                .map(|span| from_v04_span(span, &mut dict))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .expect("Failed to convert V04 spans to V05.");

    rmp_serde::to_vec(&(dict, v05_traces)).expect("Failed to serialize V05 test payload.")
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
    let span_counts = [10, 100, 500];

    let mut group = c.benchmark_group(group_name);

    for &spans_per_trace in &span_counts {
        for (sharing_label, unique_per_span) in [("high_sharing", false), ("low_sharing", true)] {
            let data = build_v05_payload(NUM_TRACES, spans_per_trace, unique_per_span);
            let data_as_bytes = libdd_tinybytes::Bytes::copy_from_slice(&data);

            group.throughput(Throughput::Elements((spans_per_trace * NUM_TRACES) as u64));
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
