// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::Temporality as SdkTemporality;

use crate::aggregator::{build_metric_exporter, Counters};
use crate::config::{OtlpExporterConfig, Temporality};
use crate::error::BuildWarning;
use crate::ExportCounters;

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
}

impl DatadogMetricExporter {
    /// Snapshot of export telemetry counters accumulated so far.
    pub fn counters(&self) -> ExportCounters {
        self.counters.snapshot()
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
        let result = self.inner.export(metrics).await;
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
/// Must be called from within a tokio runtime: the underlying `opentelemetry-otlp` exporter
/// initializes its transport (tonic/reqwest) and requires an active async context. dd-trace-rs
/// builds its provider inside tokio, so this is `async` rather than driving its own runtime.
pub async fn build_datadog_metric_exporter(
    config: &OtlpExporterConfig,
    temporality: Temporality,
) -> Result<DatadogMetricExporter, BuildWarning> {
    let inner = build_metric_exporter(config, temporality).await?;
    Ok(DatadogMetricExporter {
        inner,
        counters: Arc::new(Counters::default()),
    })
}
