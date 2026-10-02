// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use libdd_otel_telemetry::{
    InstrumentDescriptor, InstrumentKind, KeyValue, OtelMetricsAggregatorBuilder,
};
use libdd_shared_runtime::BasicRuntime;
use libdd_shared_runtime::SharedRuntime;

#[test]
fn register_and_record_without_an_exporter_never_panics() {
    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (aggregator, warnings) = OtelMetricsAggregatorBuilder::new().build(runtime);
    assert!(
        warnings.is_empty(),
        "no exporter configured, expect no warnings"
    );

    let counter_id = aggregator.register_instrument(InstrumentDescriptor::new(
        "requests",
        InstrumentKind::Counter,
    ));
    let gauge_id = aggregator.register_instrument(InstrumentDescriptor::new(
        "queue.depth",
        InstrumentKind::ObservableGauge,
    ));

    aggregator.record_counter(counter_id, 1.0, &[KeyValue::new("route", "/health")]);
    aggregator.observe_gauge(gauge_id, 42.0, &[]);

    aggregator
        .force_flush()
        .expect("force_flush should succeed even with no reader");
    aggregator.shutdown().expect("shutdown should succeed");
}

#[cfg(feature = "grpc")]
#[test]
fn exporter_uses_retained_runtime_from_reader_thread() {
    use std::time::Duration;

    use libdd_otel_telemetry::{OtlpExporterConfig, OtlpProtocol};

    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (aggregator, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(
            OtlpExporterConfig::new("http://127.0.0.1:1", OtlpProtocol::Grpc)
                .with_timeout(Duration::from_millis(50)),
        )
        .build(runtime);
    assert!(warnings.is_empty(), "gRPC exporter should build cleanly");

    let counter_id = aggregator.register_instrument(InstrumentDescriptor::new(
        "runtime.counter",
        InstrumentKind::Counter,
    ));
    aggregator.record_counter(counter_id, 1.0, &[]);

    assert!(aggregator.force_flush().is_err());
    let counters = aggregator.export_counters();
    assert_eq!(counters.metrics_export_attempts, 1);
    assert_eq!(counters.metrics_export_failures, 1);
    let _ = aggregator.shutdown();
}

#[cfg(feature = "grpc")]
#[test]
fn observable_callback_runs_during_native_collection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use libdd_otel_telemetry::{ObservableMeasurement, OtlpExporterConfig, OtlpProtocol};

    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (aggregator, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(
            OtlpExporterConfig::new("http://127.0.0.1:1", OtlpProtocol::Grpc)
                .with_timeout(Duration::from_millis(50)),
        )
        .with_export_interval(Duration::from_secs(60))
        .build(runtime);
    assert!(warnings.is_empty(), "gRPC exporter should build cleanly");

    let invocations = Arc::new(AtomicUsize::new(0));
    let callback_invocations = Arc::clone(&invocations);
    aggregator.register_observable_instrument(
        InstrumentDescriptor::new("queue.depth", InstrumentKind::ObservableGauge),
        Arc::new(move || {
            callback_invocations.fetch_add(1, Ordering::Relaxed);
            vec![ObservableMeasurement::new(
                42.0,
                vec![KeyValue::new("pool", "default")],
            )]
        }),
    );

    let _ = aggregator.force_flush();
    assert!(invocations.load(Ordering::Relaxed) > 0);
    let _ = aggregator.shutdown();
}

#[cfg(feature = "grpc")]
#[test]
fn invalid_grpc_header_falls_back_to_a_warning() {
    use libdd_otel_telemetry::{BuildWarning, OtlpExporterConfig, OtlpProtocol};

    let (_, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(
            OtlpExporterConfig::new("http://127.0.0.1:4317", OtlpProtocol::Grpc)
                .with_header("invalid header", "value"),
        )
        .build_with_default_runtime();

    assert!(matches!(
        warnings.as_slice(),
        [BuildWarning::InvalidHeader(_)]
    ));
}

// Without the `http` feature, http/protobuf is an *unsupported* protocol and must fall back to a
// warning rather than panic. With `http` enabled the protocol is supported and a real exporter is
// built, so the "unsupported" scenario doesn't apply.
#[cfg(not(feature = "http"))]
#[test]
fn unsupported_protocol_falls_back_to_a_warning_not_a_panic() {
    use libdd_otel_telemetry::{OtlpExporterConfig, OtlpProtocol};

    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (_, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(OtlpExporterConfig::new(
            "http://localhost:4318",
            OtlpProtocol::HttpProtobuf,
        ))
        .build(runtime);

    assert_eq!(warnings.len(), 1);
}

// With the `http` feature the exporter builds a reqwest+rustls client eagerly; this exercises the
// ring crypto-provider install so a missing default provider can't panic during setup.
#[cfg(feature = "http")]
#[test]
fn http_protobuf_exporter_builds_without_panicking() {
    use libdd_otel_telemetry::{OtlpExporterConfig, OtlpProtocol};

    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (_, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(OtlpExporterConfig::new(
            "http://localhost:4318",
            OtlpProtocol::HttpProtobuf,
        ))
        .build(runtime);

    assert!(warnings.is_empty(), "http exporter should build cleanly");
}

// http/json is recognized by config parsing but not exportable; it must warn, not panic.
#[test]
fn http_json_protocol_falls_back_to_a_warning() {
    use libdd_otel_telemetry::{OtlpExporterConfig, OtlpProtocol};

    let runtime = Arc::new(BasicRuntime::new().expect("runtime"));
    let (_, warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(OtlpExporterConfig::new(
            "http://localhost:4318",
            OtlpProtocol::HttpJson,
        ))
        .build(runtime);

    assert_eq!(warnings.len(), 1);
}
