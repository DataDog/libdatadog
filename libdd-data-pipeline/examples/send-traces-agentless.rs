// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Example: send a trace directly to the Datadog agentless intake.
//!
//! Reads the API key from the `DD_API_KEY` environment variable and POSTs a
//! small trace to `https://public-trace-http-intake.logs.{DD_SITE}/v1/input`
//! (defaulting to `datadoghq.com`).
//!
//! Usage:
//!   DD_API_KEY=<key> [DD_SITE=datadoghq.eu] \
//!     cargo run --example send-traces-agentless -p libdd-data-pipeline

use clap::Parser;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::{
    TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_log::logger::{
    logger_configure_std, logger_set_log_level, LogEventLevel, StdConfig, StdTarget,
};
use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
use libdd_trace_utils::span::v04::{SpanBytes, SpanEvent, SpanLink, VecMap};
use rand::random;
use std::{collections::HashMap, sync::Arc, time::UNIX_EPOCH};

fn get_span(now: i64, trace_id: u128, span_id: u64) -> SpanBytes {
    let duration = 1_000_000 * span_id as i64;
    SpanBytes {
        trace_id,
        span_id,
        parent_id: span_id.saturating_sub(1),
        duration,
        start: now + duration,
        service: "data-pipeline-agentless-example".into(),
        name: "agentless.example".into(),
        resource: "resource".into(),
        error: 0,
        metrics: VecMap::from_iter([("_sampling_priority_v1".into(), 1.0)]),
        span_events: vec![SpanEvent {
            time_unix_nano: now as u64,
            name: "event".into(),
            attributes: HashMap::new(),
        }],
        span_links: vec![SpanLink {
            trace_id: 10101010101,
            trace_id_high: 1010101,
            span_id,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[derive(Parser)]
#[command(name = "send-traces-agentless")]
#[command(about = "Send a trace to the Datadog agentless intake")]
struct Args {
    /// Override the intake URL. Defaults to
    /// `https://public-trace-http-intake.logs.{DD_SITE}/v1/input`.
    #[arg(long = "url")]
    url: Option<String>,
}

fn main() {
    logger_configure_std(StdConfig {
        target: StdTarget::Out,
    })
    .expect("Failed to configure logger");
    logger_set_log_level(LogEventLevel::Debug).expect("Failed to set log level");

    let args = Args::parse();

    let api_key = std::env::var("DD_API_KEY")
        .expect("DD_API_KEY environment variable must be set for agentless export");
    let site = std::env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".to_string());
    let intake_url = args
        .url
        .unwrap_or_else(|| format!("https://public-trace-http-intake.logs.{site}/v1/input"));

    let shared_runtime = Arc::new(ForkSafeRuntime::new().expect("Failed to create runtime"));

    let mut builder = TraceExporter::<NativeCapabilities, _>::builder();
    builder
        .set_hostname("COMP-N661JFW6JN")
        .set_env("prod")
        .set_app_version(env!("CARGO_PKG_VERSION"))
        .set_service("data-pipeline-agentless-example")
        .set_tracer_version(env!("CARGO_PKG_VERSION"))
        .set_language("nodejs")
        .set_language_version(env!("CARGO_PKG_RUST_VERSION"))
        .set_input_format(TraceExporterInputFormat::V04)
        .set_output_format(TraceExporterOutputFormat::V04)
        .set_shared_runtime(shared_runtime.clone())
        .set_agentless_endpoint(&intake_url, &api_key);

    let exporter = builder
        .build::<NativeCapabilities>()
        .expect("Failed to build TraceExporter");

    let now = UNIX_EPOCH
        .elapsed()
        .expect("Failed to read time")
        .as_nanos() as i64;

    let trace_id = random();
    let trace: Vec<_> = (1..=3).map(|i| get_span(now, trace_id, i)).collect();
    let traces = vec![trace];

    exporter
        .send_trace_chunks(traces, None)
        .expect("Failed to send traces");
    println!("Trace sent to agentless intake at {intake_url}");

    shared_runtime
        .shutdown(None)
        .expect("Failed to shutdown runtime");
}
