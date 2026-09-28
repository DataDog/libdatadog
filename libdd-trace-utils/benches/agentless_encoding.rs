// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless JSON encoding for a representative HTTP flush: 20 five-span traces with repeated
//! runtime and HTTP metadata. A second case adds one span link to each root span.

use criterion::{black_box, criterion_group, BenchmarkId, Criterion, Throughput};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::agentless_encoder::encode_payload;
use libdd_trace_utils::span::v04::{Span, SpanLink, VecMap};
use libdd_trace_utils::span::BytesData;
use libdd_trace_utils::tracer_metadata::TracerMetadata;

const NUM_TRACES: usize = 20;
const SPANS_PER_TRACE: usize = 5;

fn bytes(value: &'static str) -> BytesString {
    BytesString::from_static(value)
}

fn metadata() -> TracerMetadata {
    TracerMetadata {
        hostname: "benchmark-host".to_string(),
        env: "production".to_string(),
        runtime_id: "f0e1d2c3-b4a5-6789-0abc-def012345678".to_string(),
        tracer_version: "1.2.3".to_string(),
        language: "nodejs".to_string(),
        language_version: "24.7.0".to_string(),
        ..Default::default()
    }
}

fn traces(with_span_links: bool) -> Vec<Vec<Span<BytesData>>> {
    let mut traces = Vec::with_capacity(NUM_TRACES);

    for trace_index in 0..NUM_TRACES {
        let trace_index = u64::try_from(trace_index).expect("NUM_TRACES must fit in u64");
        let mut spans = Vec::with_capacity(SPANS_PER_TRACE);
        let trace_id =
            ((u128::from(trace_index) + 1) << 64) | (100_000_000_000 + u128::from(trace_index));
        let root_span_id = 100_000_000_000 + trace_index;

        for span_index in 0..SPANS_PER_TRACE {
            let span_index = u64::try_from(span_index).expect("SPANS_PER_TRACE must fit in u64");
            let span_id = root_span_id + span_index;
            let parent_id = if span_index == 0 { 0 } else { root_span_id };
            let span_links = if with_span_links && span_index == 0 {
                vec![SpanLink {
                    trace_id: root_span_id + 1,
                    trace_id_high: trace_index + 1,
                    span_id: root_span_id + 2,
                    attributes: [(bytes("link.name"), bytes("scheduled_by"))].into(),
                    flags: 1,
                    tracestate: bytes("dd=s:1"),
                }]
            } else {
                Vec::new()
            };

            let mut span = Span {
                service: bytes("benchmark-service"),
                name: bytes("http.request"),
                resource: bytes("GET /api/v1/resource"),
                r#type: bytes("web"),
                trace_id,
                span_id,
                parent_id,
                start: 1_700_000_000_000_000_000,
                duration: 123_456,
                meta: VecMap::from_iter([
                    (bytes("component"), bytes("http")),
                    (bytes("span.kind"), bytes("server")),
                    (bytes("http.method"), bytes("GET")),
                    (bytes("http.status_code"), bytes("200")),
                    (bytes("peer.hostname"), bytes("api.example.com")),
                    (
                        bytes("runtime-id"),
                        bytes("f0e1d2c3-b4a5-6789-0abc-def012345678"),
                    ),
                ]),
                metrics: VecMap::from_iter([
                    (bytes("_sampling_priority_v1"), 1.0),
                    (bytes("_dd.measured"), 1.0),
                    (bytes("process_id"), 1234.0),
                ]),
                span_links,
                ..Default::default()
            };
            span.dedup();
            spans.push(span);
        }
        traces.push(spans);
    }

    traces
}

fn agentless_encoding_benches(c: &mut Criterion) {
    let metadata = metadata();
    let mut group = c.benchmark_group("agentless_encoding");
    let span_count =
        u64::try_from(NUM_TRACES * SPANS_PER_TRACE).expect("benchmark span count must fit in u64");
    group.throughput(Throughput::Elements(span_count));

    for (name, with_span_links) in [("common_http", false), ("http_with_span_links", true)] {
        let traces = traces(with_span_links);
        group.bench_with_input(BenchmarkId::new(name, "20x5"), &traces, |b, traces| {
            b.iter(|| {
                black_box(encode_payload(black_box(traces), black_box(&metadata), false).unwrap())
            });
        });
    }

    group.finish();
}

criterion_group!(agentless_encoding, agentless_encoding_benches);
