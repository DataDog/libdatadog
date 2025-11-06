// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provides an abstraction layer to hold metrics that comes from 'SendDataResult'.
use ddcommon::tag;
use libdd_telemetry::data::metrics::{MetricNamespace, MetricType};
use libdd_telemetry::metrics::ContextKey;
use libdd_telemetry::worker::TelemetryWorkerHandle;
use std::ops::Index;

/// Used as identifier to match the different metrics.
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum MetricKind {
    /// trace_api.requests metric
    ApiRequest,
    /// trace_api.errors (network) metric
    ApiErrorsNetwork,
    /// trace_api.errors (timeout) metric
    ApiErrorsTimeout,
    /// trace_api.errors (status_code) metric
    ApiErrorsStatusCode,
    /// trace_api.bytes metric
    ApiBytes,
    /// trace_api.responses metric
    ApiResponses,
    /// trace_chunks_sent metric
    ChunksSent,
    /// trace_chunks_dropped metric
    ChunksDropped,
}

/// Constants for metric names
/// These must match https://github.com/DataDog/dd-go/blob/prod/trace/apps/tracer-telemetry-intake/telemetry-metrics/static/common_metrics.json
const API_REQUEST_STR: &str = "trace_api.requests";
const API_ERRORS_STR: &str = "trace_api.errors";
const API_BYTES_STR: &str = "trace_api.bytes";
const API_RESPONSES_STR: &str = "trace_api.responses";
const CHUNKS_SENT_STR: &str = "trace_chunks_sent";
const CHUNKS_DROPPED_STR: &str = "trace_chunks_dropped";

#[derive(Debug)]
struct Metric {
    name: &'static str,
    metric_type: MetricType,
    namespace: MetricNamespace,
    tags: &'static [ddcommon::tag::Tag],
}

const METRICS: &[Metric] = &[
    Metric {
        name: API_REQUEST_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
    Metric {
        name: API_ERRORS_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"], tag!["type", "network"]],
    },
    Metric {
        name: API_ERRORS_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"], tag!["type", "timeout"]],
    },
    Metric {
        name: API_ERRORS_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[
            tag!["src_library", "libdatadog"],
            tag!["type", "status_code"],
        ],
    },
    Metric {
        name: API_BYTES_STR,
        metric_type: MetricType::Distribution,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
    Metric {
        name: API_RESPONSES_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
    Metric {
        name: CHUNKS_SENT_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
    Metric {
        name: CHUNKS_DROPPED_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
    Metric {
        name: CHUNKS_DROPPED_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
        tags: &[tag!["src_library", "libdatadog"]],
    },
];

/// Structure to accumulate partial results coming from sending traces to the agent.
#[derive(Debug, Default)]
pub struct Metrics(Vec<ContextKey>);

impl Index<MetricKind> for Metrics {
    type Output = ContextKey;
    fn index(&self, index: MetricKind) -> &Self::Output {
        &self.0[index as usize]
    }
}

impl Metrics {
    /// Creates a new Metrics instance
    pub fn new(worker: &TelemetryWorkerHandle) -> Self {
        let mut keys = Vec::new();
        for metric in METRICS {
            let key = worker.register_metric_context(
                metric.name.to_string(),
                metric.tags.to_vec(),
                metric.metric_type,
                true,
                metric.namespace,
            );
            keys.push(key);
        }

        Self(keys)
    }

    /// Gets the context key associated with the metric.
    pub fn get(&self, index: MetricKind) -> &ContextKey {
        &self[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_telemetry::worker::TelemetryWorkerBuilder;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn new_metrics_test() {
        let (worker, _) = TelemetryWorkerBuilder::new_fetch_host(
            "service".to_string(),
            "language".to_string(),
            "0.1".to_string(),
            "1.0".to_string(),
        )
        .spawn();

        let metrics = Metrics::new(&worker);

        assert!(!metrics.0.is_empty());
        assert_eq!(metrics.0.len(), METRICS.len());
    }
}
