// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_otel_telemetry::{InstrumentDescriptor, InstrumentKind, OtelMetricsAggregatorBuilder};
use libdd_shared_runtime::BasicRuntime;
use libdd_shared_runtime::SharedRuntime;

#[test]
fn register_and_record_without_an_exporter_never_panics() {
    let runtime = BasicRuntime::new().expect("runtime");
    let (aggregator, warnings) = OtelMetricsAggregatorBuilder::new().build(&runtime);
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

    aggregator.record_counter(
        counter_id,
        1.0,
        &[("route".to_string(), "/health".to_string())],
    );
    aggregator.observe_gauge(gauge_id, 42.0, &[]);

    aggregator
        .force_flush()
        .expect("force_flush should succeed even with no reader");
    aggregator.shutdown().expect("shutdown should succeed");
}

#[test]
fn unsupported_protocol_falls_back_to_a_warning_not_a_panic() {
    use libdd_otel_telemetry::{OtlpExporterConfig, OtlpProtocol};

    let runtime = BasicRuntime::new().expect("runtime");
    let (_, _warnings) = OtelMetricsAggregatorBuilder::new()
        .with_metrics_exporter(OtlpExporterConfig::new(
            "http://localhost:4318",
            OtlpProtocol::HttpProtobuf,
        ))
        .build(&runtime);

    // Built with default features (grpc only), so http/protobuf should warn, not panic.
    #[cfg(not(feature = "http"))]
    assert_eq!(_warnings.len(), 1);
}
