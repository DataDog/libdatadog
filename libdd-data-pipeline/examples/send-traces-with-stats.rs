// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This example exercises `enable_telemetry`, which is only available with the
// `telemetry` feature. Gate the whole body on it so the example still compiles
// (as a no-op) when the crate is built without default features.
#[cfg(feature = "telemetry")]
mod example {
    use clap::Parser;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::trace_exporter::{
        TelemetryConfig, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
    };
    use libdd_log::logger::{
        logger_configure_std, logger_set_log_level, LogEventLevel, StdConfig, StdTarget,
    };
    use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::{
        span_pool::PooledChunks,
        v04::{Span, SpanBytes, VecMap},
    };
    use std::{
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };

    fn get_span(now: i64, trace_id: u64, span_id: u64) -> SpanBytes {
        Span {
            trace_id: trace_id as u128,
            span_id,
            parent_id: span_id - 1,
            duration: trace_id as i64 % 3 * 10_000_000 + span_id as i64 * 1_000_000,
            start: now + trace_id as i64 * 1_000_000_000 + span_id as i64 * 1_000_000,
            service: BytesString::from_static("data-pipeline-test"),
            name: format!("test-name-{}", span_id % 2).into(),
            resource: format!("test-resource-{}", (span_id + trace_id) % 3).into(),
            error: if trace_id.is_multiple_of(10) { 1 } else { 0 },
            metrics: VecMap::from_iter([
                ("_sampling_priority_v1".to_string().into(), 0.0),
                ("_dd.measured".to_string().into(), 1.0),
            ]),
            ..Default::default()
        }
    }

    /// Send traces (with stats computation) to the Datadog Agent or directly to the intake.
    ///
    /// # Agent mode (default)
    ///
    ///   cargo run --example send-traces-with-stats -p libdd-data-pipeline -- --url http://localhost:8126
    ///
    /// # Agentless mode
    ///
    /// Set DD_API_KEY (and optionally DD_SITE) to bypass the Agent and send traces + stats
    /// directly to the Datadog intake:
    ///
    ///   DD_API_KEY=<key> [DD_SITE=datadoghq.eu] \
    ///     cargo run --example send-traces-with-stats -p libdd-data-pipeline
    ///
    /// Agentless endpoint defaults:
    ///   Traces : https://public-trace-http-intake.logs.{DD_SITE}/v1/input
    ///   Stats  : https://trace.agent.{DD_SITE}/api/v0.2/stats
    ///
    /// Both can be overridden with --agentless-url / --agentless-stats-url.
    #[derive(Parser)]
    #[command(name = "send-traces-with-stats")]
    #[command(about = "Send traces with statistics to the Datadog Agent or the intake (agentless)")]
    struct Args {
        /// Agent URL (agent mode only, ignored when DD_API_KEY is set).
        #[arg(
            short = 'u',
            long = "url",
            default_value = "http://localhost:8126",
            help = "Trace agent URL (agent mode). Ignored when DD_API_KEY is set.\n\nExamples:\n  http://localhost:8126 (default)\n  windows://./pipe/dd-apm-test-agent (Windows named pipe)"
        )]
        url: String,

        /// Override the agentless trace intake URL.
        /// Defaults to https://public-trace-http-intake.logs.{DD_SITE}/v1/input.
        #[arg(long = "agentless-url")]
        agentless_url: Option<String>,

        /// Override the agentless stats intake URL.
        /// Defaults to https://trace.agent.{DD_SITE}/api/v0.2/stats.
        #[arg(long = "agentless-stats-url")]
        agentless_stats_url: Option<String>,
    }

    pub fn run() {
        logger_configure_std(StdConfig {
            target: StdTarget::Out,
        })
        .expect("Failed to configure logger");
        logger_set_log_level(LogEventLevel::Debug).expect("Failed to set log level");

        let shared_runtime = Arc::new(ForkSafeRuntime::new().expect("Failed to create runtime"));

        let args = Args::parse();
        let telemetry_cfg = TelemetryConfig::default();
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_hostname("test")
            .set_env("testing")
            .set_app_version(env!("CARGO_PKG_VERSION"))
            .set_service("data-pipeline-test")
            .set_tracer_version(env!("CARGO_PKG_VERSION"))
            .set_language("rust")
            .set_language_version(env!("CARGO_PKG_RUST_VERSION"))
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_shared_runtime(shared_runtime.clone())
            .enable_telemetry(telemetry_cfg)
            .enable_stats(Duration::from_secs(10));

        // When DD_API_KEY is present, use agentless mode; otherwise fall back to the agent URL.
        if let Ok(api_key) = std::env::var("DD_API_KEY") {
            let site = std::env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".to_string());

            let trace_url = args.agentless_url.unwrap_or_else(|| {
                format!("https://public-trace-http-intake.logs.{site}/v1/input")
            });
            let stats_url = args
                .agentless_stats_url
                .unwrap_or_else(|| format!("https://trace.agent.{site}/api/v0.2/stats"));

            println!("Agentless mode");
            println!("  Trace intake : {trace_url}");
            println!("  Stats intake : {stats_url}");

            builder
                .set_agentless_endpoint(&trace_url, &api_key)
                .set_agentless_stats_endpoint(&stats_url);
        } else {
            println!("Agent mode: {}", args.url);
            builder.set_url(&args.url);
        }

        let exporter = builder
            .build::<NativeCapabilities>()
            .expect("Failed to build TraceExporter");
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

        dbg!(&traces);

        exporter
            .send_trace_chunks(PooledChunks::unpooled(traces), None)
            .expect("Failed to send traces");
        shared_runtime
            .shutdown(None)
            .expect("Failed to shutdown runtime");
    }
}

#[cfg(feature = "telemetry")]
fn main() {
    example::run();
}

#[cfg(not(feature = "telemetry"))]
fn main() {
    eprintln!(
        "This example requires the `telemetry` feature. Rebuild with \
         `--features telemetry` (enabled by default)."
    );
}
