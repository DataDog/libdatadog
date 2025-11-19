// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use libdd_data_pipeline::trace_exporter::{
    TelemetryConfig, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_log::logger::{
    logger_configure_std, logger_set_log_level, LogEventLevel, StdConfig, StdTarget,
};
use libdd_trace_protobuf::pb;
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

#[derive(Parser)]
#[command(name = "send-traces-with-stats")]
#[command(about = "A data pipeline example for sending traces with statistics")]
struct Args {
    #[arg(
        short = 'u',
        long = "url",
        default_value = "http://localhost:8126",
        help = "Set the trace agent URL\n\nExamples:\n  http://localhost:8126 (default)\n  windows://./pipe/dd-apm-test-agent (Windows named pipe)\n  https://trace.agent.datadoghq.com:443 (custom endpoint)"
    )]
    url: String,
}

fn main() {
    logger_configure_std(StdConfig {
        target: StdTarget::Out,
    })
    .expect("Failed to configure logger");
    logger_set_log_level(LogEventLevel::Debug).expect("Failed to set log level");

    let args = Args::parse();
    let telemetry_cfg = TelemetryConfig::default();
    let mut builder = TraceExporter::builder();
    builder
        .set_url(&args.url)
        .set_hostname("test")
        .set_env("testing")
        .set_app_version(env!("CARGO_PKG_VERSION"))
        .set_service("data-pipeline-test")
        .set_tracer_version(env!("CARGO_PKG_VERSION"))
        .set_language("rust")
        .set_language_version(env!("CARGO_PKG_RUST_VERSION"))
        .set_input_format(TraceExporterInputFormat::V04)
        .set_output_format(TraceExporterOutputFormat::V04)
        .enable_telemetry(telemetry_cfg)
        .enable_stats(Duration::from_secs(10));
    let exporter = builder.build_tokio().expect("Failed to build TraceExporter");
    let now = UNIX_EPOCH
        .elapsed()
        .expect("Failed to get time since UNIX_EPOCH")
        .as_nanos() as i64;

    let mut traces = Vec::new();
    for trace_id in 1..=2 {
        let mut trace = Vec::new();
        for span_id in 1..=2 {
            trace.push(get_span(now, trace_id, span_id));
        }
        traces.push(trace);
    }
    let data = rmp_serde::to_vec_named(&traces).expect("Failed to serialize traces");

    exporter
        .send(data.as_ref(), 2)
        .expect("Failed to send traces");
    exporter
        .shutdown(None)
        .expect("Failed to shutdown exporter");
}
