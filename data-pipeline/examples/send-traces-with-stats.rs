// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::{
    TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use datadog_trace_protobuf::pb;
use std::{
    collections::HashMap,
    time::{Duration, UNIX_EPOCH},
};

fn get_span(now: i64, trace_id: u64, span_id: u64) -> pb::Span {
    pb::Span {
        trace_id,
        span_id,
        parent_id: span_id - 1,
        duration: trace_id as i64 % 3 * 10_000_000 + span_id as i64 * 1_000_000,
        start: now + trace_id as i64 * 1_000_000_000 + span_id as i64 * 1_000_000,
        service: "data-pipeline-test".to_string(),
        name: format!("test-name-{}", span_id % 2),
        resource: format!("test-resource-{}", (span_id + trace_id) % 3),
        error: if trace_id % 10 == 0 { 1 } else { 0 },
        metrics: HashMap::from([
            ("_sampling_priority_v1".to_string(), 1.0),
            ("_dd.measured".to_string(), 1.0),
        ]),
        ..Default::default()
    }
}

fn main() {
    let exporter = TraceExporter::builder()
        .set_url("http://localhost:8126")
        .set_hostname("test")
        .set_env("testing")
        .set_app_version(env!("CARGO_PKG_VERSION"))
        .set_service("data-pipeline-test")
        .set_tracer_version(env!("CARGO_PKG_VERSION"))
        .set_language("rust")
        .set_language_version(env!("CARGO_PKG_RUST_VERSION"))
        .set_input_format(TraceExporterInputFormat::V04)
        .set_output_format(TraceExporterOutputFormat::V07)
        .enable_stats(Duration::from_secs(10))
        .build()
        .unwrap();
    let now = UNIX_EPOCH.elapsed().unwrap().as_nanos() as i64;

    let mut traces = Vec::new();
    for trace_id in 1..=100 {
        let mut trace = Vec::new();
        for span_id in 1..=1000 {
            trace.push(get_span(now, trace_id, span_id));
        }
        traces.push(trace);
    }
    let data = rmp_serde::to_vec_named(&traces).unwrap();
    let data_as_bytes = tinybytes::Bytes::from(data);

    exporter.send(data_as_bytes, 100).unwrap();
    exporter.shutdown(None).unwrap();
}
