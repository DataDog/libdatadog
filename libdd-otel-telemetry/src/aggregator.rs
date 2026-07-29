// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libdd_shared_runtime::BlockingRuntime;
use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider, UpDownCounter};
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::Resource;

use crate::config::{OtlpExporterConfig, OtlpProtocol, Temporality};
use crate::error::{BuildWarning, OtelMetricsError};
use crate::instrument::{InstrumentDescriptor, InstrumentId, InstrumentKind};

/// Snapshot of export attempt counters, polled by the host tracer to feed its own telemetry
/// system. Deliberately a plain data struct rather than a callback: nothing that isn't a
/// primitive crosses the aggregator's public boundary in either direction.
///
/// NOTE: the counting exporter wrapper (`PushMetricExporter` decorator incrementing these on
/// every export attempt, mirroring dd-trace-rs's `TelemetryTrackingExporter`) is not implemented
/// yet — `export_counters()` currently always returns zeros. Follow-up before Phase 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportCounters {
    pub metrics_export_attempts: u64,
    pub metrics_export_successes: u64,
    pub metrics_export_failures: u64,
}

#[derive(Debug, Default)]
struct Counters {
    attempts: AtomicU64,
    successes: AtomicU64,
    failures: AtomicU64,
}

enum InstrumentHandle {
    Counter(Counter<f64>),
    UpDownCounter(UpDownCounter<f64>),
    Histogram(Histogram<f64>),
    Gauge(Gauge<f64>),
}

/// Builds a [`OtelMetricsAggregator`].
///
/// Never fails outright: any misconfiguration (bad endpoint, unsupported protocol, exporter init
/// failure) is captured as a [`BuildWarning`] and the resulting aggregator silently drops
/// everything it's given instead — a misconfigured OTel pipeline must never prevent the host
/// tracer from starting.
pub struct OtelMetricsAggregatorBuilder {
    resource: Resource,
    metrics_exporter: Option<OtlpExporterConfig>,
    temporality: Temporality,
    export_interval: Duration,
}

impl Default for OtelMetricsAggregatorBuilder {
    fn default() -> Self {
        Self {
            resource: Resource::builder().build(),
            metrics_exporter: None,
            temporality: Temporality::default(),
            export_interval: Duration::from_secs(60),
        }
    }
}

impl OtelMetricsAggregatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.resource = resource;
        self
    }

    pub fn with_metrics_exporter(mut self, config: OtlpExporterConfig) -> Self {
        self.metrics_exporter = Some(config);
        self
    }

    pub fn with_metrics_temporality(mut self, temporality: Temporality) -> Self {
        self.temporality = temporality;
        self
    }

    pub fn with_export_interval(mut self, interval: Duration) -> Self {
        self.export_interval = interval;
        self
    }

    /// Builds the aggregator, driving exporter construction on `runtime` since the OTLP
    /// exporters require an active async context to initialize their transport.
    pub fn build<R: BlockingRuntime>(
        self,
        runtime: &R,
    ) -> (OtelMetricsAggregator, Vec<BuildWarning>) {
        let mut warnings = Vec::new();

        let reader = match &self.metrics_exporter {
            Some(cfg) => match runtime.block_on(build_metric_exporter(cfg, self.temporality)) {
                Ok(Ok(exporter)) => Some(
                    PeriodicReader::builder(exporter)
                        .with_interval(self.export_interval)
                        .build(),
                ),
                Ok(Err(warning)) => {
                    warnings.push(warning);
                    None
                }
                Err(_) => {
                    warnings.push(BuildWarning::ExporterInitFailed(
                        "runtime unavailable while building metrics exporter".to_string(),
                    ));
                    None
                }
            },
            None => None,
        };

        let mut provider_builder = SdkMeterProvider::builder().with_resource(self.resource);
        if let Some(reader) = reader {
            provider_builder = provider_builder.with_reader(reader);
        }
        let provider = provider_builder.build();
        let meter = provider.meter("libdd-otel-telemetry");

        let aggregator = OtelMetricsAggregator {
            provider,
            meter,
            instruments: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            counters: Arc::new(Counters::default()),
        };
        (aggregator, warnings)
    }
}

async fn build_metric_exporter(
    config: &OtlpExporterConfig,
    temporality: Temporality,
) -> Result<opentelemetry_otlp::MetricExporter, BuildWarning> {
    use opentelemetry_otlp::WithExportConfig;

    let result = match config.protocol {
        #[cfg(feature = "grpc")]
        OtlpProtocol::Grpc => opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(&config.endpoint)
            .with_timeout(config.timeout)
            .with_temporality(temporality.into())
            .build(),
        #[cfg(not(feature = "grpc"))]
        OtlpProtocol::Grpc => {
            return Err(BuildWarning::UnsupportedProtocol(
                "grpc protocol requires the 'grpc' feature".to_string(),
            ))
        }
        #[cfg(feature = "http")]
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(&config.endpoint)
            .with_timeout(config.timeout)
            .with_temporality(temporality.into())
            .build(),
        #[cfg(not(feature = "http"))]
        OtlpProtocol::HttpProtobuf => {
            return Err(BuildWarning::UnsupportedProtocol(
                "http/protobuf protocol requires the 'http' feature".to_string(),
            ))
        }
    };

    result.map_err(|e| BuildWarning::ExporterInitFailed(e.to_string()))
}

/// Aggregates primitive metric observations from a host tracer and exports them via OTLP.
///
/// This is the entire public surface a host language binds to: register an instrument once, then
/// push resolved primitive values for it. The aggregator does not know or care whether a value
/// came from a synchronous instrument call or from a host-language-scheduled observable-instrument
/// callback — both are just "a value for this instrument id."
pub struct OtelMetricsAggregator {
    provider: SdkMeterProvider,
    meter: opentelemetry::metrics::Meter,
    instruments: Mutex<HashMap<InstrumentId, InstrumentHandle>>,
    next_id: AtomicU64,
    counters: Arc<Counters>,
}

impl OtelMetricsAggregator {
    pub fn register_instrument(&self, descriptor: InstrumentDescriptor) -> InstrumentId {
        let id = InstrumentId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let handle = self.create_instrument(&descriptor);
        self.instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, handle);
        id
    }

    fn create_instrument(&self, descriptor: &InstrumentDescriptor) -> InstrumentHandle {
        let name = descriptor.name.clone();
        match descriptor.kind {
            InstrumentKind::Counter | InstrumentKind::ObservableCounter => {
                let mut builder = self.meter.f64_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::Counter(builder.build())
            }
            InstrumentKind::UpDownCounter | InstrumentKind::ObservableUpDownCounter => {
                let mut builder = self.meter.f64_up_down_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::UpDownCounter(builder.build())
            }
            InstrumentKind::Histogram => {
                let mut builder = self.meter.f64_histogram(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::Histogram(builder.build())
            }
            InstrumentKind::ObservableGauge => {
                let mut builder = self.meter.f64_gauge(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::Gauge(builder.build())
            }
        }
    }

    fn attrs(pairs: &[(String, String)]) -> Vec<KeyValue> {
        pairs
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
            .collect()
    }

    pub fn record_counter(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        if let Some(InstrumentHandle::Counter(counter)) = self
            .instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            counter.add(value, &Self::attrs(attrs));
        }
    }

    pub fn record_up_down_counter(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        if let Some(InstrumentHandle::UpDownCounter(counter)) = self
            .instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            counter.add(value, &Self::attrs(attrs));
        }
    }

    pub fn record_histogram(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        if let Some(InstrumentHandle::Histogram(histogram)) = self
            .instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            histogram.record(value, &Self::attrs(attrs));
        }
    }

    /// Pushes a resolved value for an observable gauge. The host language is responsible for
    /// deciding when to evaluate the user's callback; this only records the result.
    pub fn observe_gauge(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        if let Some(InstrumentHandle::Gauge(gauge)) = self
            .instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            gauge.record(value, &Self::attrs(attrs));
        }
    }

    /// Pushes a resolved value for an observable counter, same caveat as [`Self::observe_gauge`].
    pub fn observe_counter(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        self.record_counter(id, value, attrs);
    }

    /// Snapshot of export telemetry counters accumulated so far. Poll this after `force_flush`
    /// or on your own interval to report into your own telemetry system.
    pub fn export_counters(&self) -> ExportCounters {
        ExportCounters {
            metrics_export_attempts: self.counters.attempts.load(Ordering::Relaxed),
            metrics_export_successes: self.counters.successes.load(Ordering::Relaxed),
            metrics_export_failures: self.counters.failures.load(Ordering::Relaxed),
        }
    }

    pub fn force_flush(&self) -> Result<(), OtelMetricsError> {
        self.provider
            .force_flush()
            .map_err(|e| OtelMetricsError(e.to_string()))
    }

    pub fn shutdown(self) -> Result<(), OtelMetricsError> {
        self.provider
            .shutdown()
            .map_err(|e| OtelMetricsError(e.to_string()))
    }
}
