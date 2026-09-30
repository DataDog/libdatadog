// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use libdd_shared_runtime::BlockingRuntime;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::metrics::Temporality as SdkTemporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

use crate::ExportCounters;
use crate::aggregator::{Counters, build_metric_exporter};
use crate::config::{OtlpExporterConfig, Temporality};
use crate::error::BuildWarning;

/// A Datadog-flavored OTLP [`PushMetricExporter`] that a host tracer can plug into its own
/// `SdkMeterProvider` + `PeriodicReader`.
///
/// It wraps an upstream [`opentelemetry_otlp::MetricExporter`] and tracks export attempts,
/// successes, and failures — the centralized equivalent of dd-trace-rs's old
/// `TelemetryTrackingExporter`. Poll [`DatadogMetricExporter::counters`] to feed those counts
/// into your own telemetry system.
pub struct DatadogMetricExporter {
    inner: opentelemetry_otlp::MetricExporter,
    counters: Arc<Counters>,
    runtime: Arc<dyn MetricExporterRuntime>,
}

trait MetricExporterRuntime: Send + Sync {
    fn export(
        &self,
        exporter: &opentelemetry_otlp::MetricExporter,
        metrics: &ResourceMetrics,
    ) -> OTelSdkResult;
}

impl<R> MetricExporterRuntime for R
where
    R: BlockingRuntime + Send + Sync,
{
    fn export(
        &self,
        exporter: &opentelemetry_otlp::MetricExporter,
        metrics: &ResourceMetrics,
    ) -> OTelSdkResult {
        self.block_on(async { exporter.export(metrics).await })
            .map_err(|error| OTelSdkError::InternalFailure(error.to_string()))?
    }
}

impl DatadogMetricExporter {
    /// Snapshot of export telemetry counters accumulated so far.
    pub fn counters(&self) -> ExportCounters {
        self.counters.snapshot()
    }

    pub(crate) fn counters_handle(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }
}

impl std::fmt::Debug for DatadogMetricExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatadogMetricExporter")
            .field("counters", &self.counters.snapshot())
            .finish()
    }
}

impl PushMetricExporter for DatadogMetricExporter {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        self.counters.attempts.fetch_add(1, Ordering::Relaxed);
        let result = self.runtime.export(&self.inner, metrics);
        match &result {
            Ok(()) => self.counters.successes.fetch_add(1, Ordering::Relaxed),
            Err(_) => self.counters.failures.fetch_add(1, Ordering::Relaxed),
        };
        result
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> SdkTemporality {
        self.inner.temporality()
    }
}

/// Builds a [`DatadogMetricExporter`] from an [`OtlpExporterConfig`].
///
/// `runtime` drives every export because the upstream SDK invokes exporters from a plain worker
/// thread with no async reactor.
pub fn build_datadog_metric_exporter<R>(
    config: &OtlpExporterConfig,
    temporality: Temporality,
    runtime: Arc<R>,
) -> Result<DatadogMetricExporter, BuildWarning>
where
    R: BlockingRuntime + Send + Sync + 'static,
{
    let inner = runtime
        .block_on(async { build_metric_exporter(config, temporality) })
        .map_err(|error| BuildWarning::ExporterInitFailed(error.to_string()))??;
    Ok(DatadogMetricExporter {
        inner,
        counters: Arc::new(Counters::default()),
        runtime,
    })
}
