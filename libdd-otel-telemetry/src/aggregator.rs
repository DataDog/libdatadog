// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libdd_shared_runtime::{BasicRuntime, BlockingRuntime, SharedRuntime};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{
    Counter, Gauge, Histogram, MeterProvider, ObservableCounter, ObservableGauge,
    ObservableUpDownCounter, UpDownCounter,
};
#[cfg(any(feature = "grpc", feature = "http"))]
use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::metrics::SdkMeterProvider;

#[cfg(any(feature = "grpc", feature = "http"))]
use crate::config::OtlpProtocol;
use crate::config::{OtlpExporterConfig, Temporality};
use crate::error::{BuildWarning, OtelMetricsError};
use crate::instrument::{InstrumentDescriptor, InstrumentId, InstrumentKind, ObservableCallback};
use crate::resource::ResourceBuilder;

/// Snapshot of export attempt counters, polled by the host tracer to feed its own telemetry
/// system. Deliberately a plain data struct rather than a callback: nothing that isn't a
/// primitive crosses the aggregator's public boundary in either direction.
///
/// The counting is performed by [`crate::DatadogMetricExporter`], which increments these on every
/// export attempt (mirroring dd-trace-rs's old `TelemetryTrackingExporter`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportCounters {
    pub metrics_export_attempts: u64,
    pub metrics_export_successes: u64,
    pub metrics_export_failures: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) attempts: AtomicU64,
    pub(crate) successes: AtomicU64,
    pub(crate) failures: AtomicU64,
}

impl Counters {
    /// Reads the current counter values into a public [`ExportCounters`] snapshot.
    pub(crate) fn snapshot(&self) -> ExportCounters {
        ExportCounters {
            metrics_export_attempts: self.attempts.load(Ordering::Relaxed),
            metrics_export_successes: self.successes.load(Ordering::Relaxed),
            metrics_export_failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

enum InstrumentHandle {
    Counter(Counter<f64>),
    UpDownCounter(UpDownCounter<f64>),
    Histogram(Histogram<f64>),
    Gauge(Gauge<f64>),
    ObservableCounter(ObservableCounter<f64>),
    ObservableGauge(ObservableGauge<f64>),
    ObservableUpDownCounter(ObservableUpDownCounter<f64>),
}

/// Builds a [`OtelMetricsAggregator`].
///
/// Never fails outright: any misconfiguration (bad endpoint, unsupported protocol, exporter init
/// failure) is captured as a [`BuildWarning`] and the resulting aggregator silently drops
/// everything it's given instead — a misconfigured OTel pipeline must never prevent the host
/// tracer from starting.
pub struct OtelMetricsAggregatorBuilder {
    resource: ResourceBuilder,
    metrics_exporter: Option<OtlpExporterConfig>,
    temporality: Temporality,
    export_interval: Duration,
}

impl Default for OtelMetricsAggregatorBuilder {
    fn default() -> Self {
        Self {
            resource: ResourceBuilder::new(),
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

    pub fn with_resource(mut self, resource: ResourceBuilder) -> Self {
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

    /// Builds with a runtime owned by this crate, avoiding runtime types across FFI boundaries.
    pub fn build_with_default_runtime(self) -> (OtelMetricsAggregator, Vec<BuildWarning>) {
        match BasicRuntime::new() {
            Ok(runtime) => self.build(Arc::new(runtime)),
            Err(error) => {
                let warning = BuildWarning::ExporterInitFailed(error.to_string());
                let provider = SdkMeterProvider::builder()
                    .with_resource(self.resource.build())
                    .build();
                (
                    OtelMetricsAggregator::new(provider, Arc::new(Counters::default())),
                    vec![warning],
                )
            }
        }
    }

    /// Builds the aggregator, retaining `runtime` to drive exports from the SDK reader thread.
    pub fn build<R>(self, _runtime: Arc<R>) -> (OtelMetricsAggregator, Vec<BuildWarning>)
    where
        R: BlockingRuntime + Send + Sync + 'static,
    {
        let mut warnings = Vec::new();
        let counters = Arc::new(Counters::default());

        #[cfg(any(feature = "grpc", feature = "http"))]
        let (reader, counters) = match &self.metrics_exporter {
            Some(cfg) => {
                match crate::exporter::build_datadog_metric_exporter_with_runtime(
                    cfg,
                    self.temporality,
                    Arc::clone(&_runtime),
                ) {
                    Ok(exporter) => {
                        let counters = exporter.counters_handle();
                        let reader = Some(
                            PeriodicReader::builder(exporter)
                                .with_interval(self.export_interval)
                                .build(),
                        );
                        (reader, counters)
                    }
                    Err(warning) => {
                        warnings.push(warning);
                        (None, counters)
                    }
                }
            }
            None => (None, counters),
        };

        #[cfg(not(any(feature = "grpc", feature = "http")))]
        if self.metrics_exporter.is_some() {
            warnings.push(BuildWarning::UnsupportedProtocol(
                "metrics export requires the 'grpc' or 'http' feature".to_string(),
            ));
        }

        let provider_builder = SdkMeterProvider::builder().with_resource(self.resource.build());
        #[cfg(any(feature = "grpc", feature = "http"))]
        let provider_builder = match reader {
            Some(reader) => provider_builder.with_reader(reader),
            None => provider_builder,
        };
        let provider = provider_builder.build();

        let aggregator = OtelMetricsAggregator::new(provider, counters);
        (aggregator, warnings)
    }
}

#[cfg(any(feature = "grpc", feature = "http"))]
pub(crate) fn build_metric_exporter(
    config: &OtlpExporterConfig,
    temporality: Temporality,
) -> Result<opentelemetry_otlp::MetricExporter, BuildWarning> {
    use opentelemetry_otlp::WithExportConfig;
    #[cfg(feature = "http")]
    use opentelemetry_otlp::WithHttpConfig;
    #[cfg(feature = "grpc")]
    use opentelemetry_otlp::WithTonicConfig;

    let result = match config.protocol {
        #[cfg(feature = "grpc")]
        OtlpProtocol::Grpc => {
            let mut headers = http::HeaderMap::new();
            for (key, value) in &config.headers {
                let name = http::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|error| BuildWarning::InvalidHeader(error.to_string()))?;
                let value = http::header::HeaderValue::from_str(value)
                    .map_err(|error| BuildWarning::InvalidHeader(error.to_string()))?;
                headers.insert(name, value);
            }
            opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(&config.endpoint)
                .with_timeout(config.timeout)
                .with_metadata(
                    opentelemetry_otlp::tonic_types::metadata::MetadataMap::from_headers(headers),
                )
                .with_temporality(temporality.into())
                .build()
        }
        #[cfg(not(feature = "grpc"))]
        OtlpProtocol::Grpc => {
            return Err(BuildWarning::UnsupportedProtocol(
                "grpc protocol requires the 'grpc' feature".to_string(),
            ));
        }
        #[cfg(feature = "http")]
        OtlpProtocol::HttpProtobuf => {
            // reqwest+rustls 0.23 has no process-default crypto provider under feature
            // unification and panics ("no process-level CryptoProvider available") when it
            // builds a TLS client. libdatadog standardizes on ring, so best-effort install it
            // before the exporter constructs its client. Ignoring the result is intentional:
            // it errors only if a provider is already set, which is fine.
            let _ = rustls::crypto::ring::default_provider().install_default();
            opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(&config.endpoint)
                .with_timeout(config.timeout)
                .with_headers(config.headers.iter().cloned().collect())
                .with_temporality(temporality.into())
                .build()
        }
        #[cfg(not(feature = "http"))]
        OtlpProtocol::HttpProtobuf => {
            return Err(BuildWarning::UnsupportedProtocol(
                "http/protobuf protocol requires the 'http' feature".to_string(),
            ));
        }
        OtlpProtocol::HttpJson => {
            return Err(BuildWarning::UnsupportedProtocol(
                "http/json protocol is not supported for OTLP metrics export".to_string(),
            ));
        }
    };

    result.map_err(|e| BuildWarning::ExporterInitFailed(e.to_string()))
}

/// Identifies an OpenTelemetry instrumentation scope.
type MeterScope = (
    String,
    Option<String>,
    Option<String>,
    Vec<(String, String)>,
);

/// Aggregates primitive metric observations from a host tracer and exports them via OTLP.
///
/// This is the entire public surface a host language binds to: register an instrument once, then
/// push synchronous primitive values or provide an observable callback that returns them. The
/// native reader invokes observable callbacks as part of collection.
pub struct OtelMetricsAggregator {
    provider: SdkMeterProvider,
    meters: Mutex<HashMap<MeterScope, opentelemetry::metrics::Meter>>,
    instruments: Mutex<HashMap<InstrumentId, InstrumentHandle>>,
    next_id: AtomicU64,
    counters: Arc<Counters>,
}

impl OtelMetricsAggregator {
    fn new(provider: SdkMeterProvider, counters: Arc<Counters>) -> Self {
        Self {
            provider,
            meters: Mutex::new(HashMap::new()),
            instruments: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            counters,
        }
    }

    pub fn register_instrument(&self, descriptor: InstrumentDescriptor) -> InstrumentId {
        let id = InstrumentId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let handle = self.create_instrument(&descriptor);
        self.instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, handle);
        id
    }

    pub fn register_observable_instrument(
        &self,
        descriptor: InstrumentDescriptor,
        callback: ObservableCallback,
    ) -> InstrumentId {
        let id = InstrumentId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let handle = self.create_observable_instrument(&descriptor, callback);
        self.instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, handle);
        id
    }

    /// Returns the SDK `Meter` for the descriptor's instrumentation scope, creating and caching it
    /// on first use so every exported metric carries the host's `get_meter` identity.
    fn meter_for(&self, descriptor: &InstrumentDescriptor) -> opentelemetry::metrics::Meter {
        let key: MeterScope = (
            descriptor.meter_name.clone(),
            descriptor.meter_version.clone(),
            descriptor.meter_schema_url.clone(),
            descriptor.meter_attributes.clone(),
        );
        let mut meters = self.meters.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(meter) = meters.get(&key) {
            return meter.clone();
        }
        let mut scope = opentelemetry::InstrumentationScope::builder(descriptor.meter_name.clone());
        if let Some(version) = &descriptor.meter_version {
            scope = scope.with_version(version.clone());
        }
        if let Some(schema_url) = &descriptor.meter_schema_url {
            scope = scope.with_schema_url(schema_url.clone());
        }
        scope = scope.with_attributes(Self::attrs(&descriptor.meter_attributes));
        let meter = self.provider.meter_with_scope(scope.build());
        meters.insert(key, meter.clone());
        meter
    }

    fn create_instrument(&self, descriptor: &InstrumentDescriptor) -> InstrumentHandle {
        let meter = self.meter_for(descriptor);
        let name = descriptor.name.clone();
        match descriptor.kind {
            InstrumentKind::Counter | InstrumentKind::ObservableCounter => {
                let mut builder = meter.f64_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::Counter(builder.build())
            }
            InstrumentKind::UpDownCounter | InstrumentKind::ObservableUpDownCounter => {
                let mut builder = meter.f64_up_down_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::UpDownCounter(builder.build())
            }
            InstrumentKind::Histogram => {
                let mut builder = meter.f64_histogram(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::Histogram(builder.build())
            }
            InstrumentKind::ObservableGauge => {
                let mut builder = meter.f64_gauge(name);
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

    fn create_observable_instrument(
        &self,
        descriptor: &InstrumentDescriptor,
        callback: ObservableCallback,
    ) -> InstrumentHandle {
        let meter = self.meter_for(descriptor);
        let name = descriptor.name.clone();
        match descriptor.kind {
            InstrumentKind::ObservableCounter => {
                let mut builder = meter.f64_observable_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::ObservableCounter(
                    builder
                        .with_callback(move |observer| {
                            for measurement in callback() {
                                observer.observe(
                                    measurement.value,
                                    &Self::attrs(&measurement.attributes),
                                );
                            }
                        })
                        .build(),
                )
            }
            InstrumentKind::ObservableGauge => {
                let mut builder = meter.f64_observable_gauge(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::ObservableGauge(
                    builder
                        .with_callback(move |observer| {
                            for measurement in callback() {
                                observer.observe(
                                    measurement.value,
                                    &Self::attrs(&measurement.attributes),
                                );
                            }
                        })
                        .build(),
                )
            }
            InstrumentKind::ObservableUpDownCounter => {
                let mut builder = meter.f64_observable_up_down_counter(name);
                if let Some(unit) = &descriptor.unit {
                    builder = builder.with_unit(unit.clone());
                }
                if let Some(description) = &descriptor.description {
                    builder = builder.with_description(description.clone());
                }
                InstrumentHandle::ObservableUpDownCounter(
                    builder
                        .with_callback(move |observer| {
                            for measurement in callback() {
                                observer.observe(
                                    measurement.value,
                                    &Self::attrs(&measurement.attributes),
                                );
                            }
                        })
                        .build(),
                )
            }
            _ => self.create_instrument(descriptor),
        }
    }

    fn attrs(pairs: &[(String, String)]) -> Vec<KeyValue> {
        pairs
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
            .collect()
    }

    pub fn record_counter(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        if value < 0.0 {
            return;
        }
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
        if value < 0.0 {
            return;
        }
        if let Some(InstrumentHandle::Histogram(histogram)) = self
            .instruments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            histogram.record(value, &Self::attrs(attrs));
        }
    }

    /// Pushes a resolved value for a synchronous gauge.
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

    /// Pushes a resolved value for a synchronous counter backed by the counter handle.
    pub fn observe_counter(&self, id: InstrumentId, value: f64, attrs: &[(String, String)]) {
        self.record_counter(id, value, attrs);
    }

    /// Snapshot of export telemetry counters accumulated so far. Poll this after `force_flush`
    /// or on your own interval to report into your own telemetry system.
    pub fn export_counters(&self) -> ExportCounters {
        self.counters.snapshot()
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
