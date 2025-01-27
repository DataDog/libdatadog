// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provides an abstraction layer to hold metrics that comes from 'SendDataResult'.
use ddcommon::tag;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::TelemetryWorkerHandle;
use std::collections::HashMap;

/// trace_api.requests metric
pub const API_REQUEST_STR: &str = "trace_api.requests";
/// trace_api.errors metric
pub const API_ERRORS_STR: &str = "trace_api.errors";
/// trace_api.bytes metric
pub const API_BYTES_STR: &str = "trace_api.bytes";
/// trace_api.responses metric
pub const API_RESPONSES_STR: &str = "trace_api.responses";
/// trace_chunk_sent metric
pub const CHUNKS_SENT_STR: &str = "trace_chunk_sent";
/// trace_chunk_dropped metric
pub const CHUNKS_DROPPED_STR: &str = "trace_chunk_dropped";

struct Metric {
    name: &'static str,
    metric_type: MetricType,
    namespace: MetricNamespace,
}

const METRICS: &[Metric] = &[
    Metric {
        name: API_REQUEST_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
    },
    Metric {
        name: API_ERRORS_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
    },
    Metric {
        name: API_BYTES_STR,
        metric_type: MetricType::Distribution,
        namespace: MetricNamespace::Tracers,
    },
    Metric {
        name: API_RESPONSES_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
    },
    Metric {
        name: CHUNKS_SENT_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
    },
    Metric {
        name: CHUNKS_DROPPED_STR,
        metric_type: MetricType::Count,
        namespace: MetricNamespace::Tracers,
    },
];

/// Structure to accumulate partial results coming from sending traces to the agent.
#[derive(Debug)]
pub struct Metrics(HashMap<String, ContextKey>);

impl Metrics {
    /// Creates a new Metrics instance
    pub fn new(worker: &TelemetryWorkerHandle) -> Self {
        let mut map = HashMap::new();
        for metric in METRICS {
            let key = worker.register_metric_context(
                metric.name.to_string(),
                vec![tag!("src_library", "libdatadog")],
                metric.metric_type,
                true,
                metric.namespace,
            );
            map.insert(metric.name.to_string(), key);
        }

        Self(map)
    }

    /// Gets the context key associated with the metric.
    pub fn get(&self, metric_name: &str) -> Option<&ContextKey> {
        self.0.get(metric_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddtelemetry::config::Config;
    use ddtelemetry::worker::TelemetryWorkerBuilder;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn new_test() {
        let (worker, _) = TelemetryWorkerBuilder::new_fetch_host(
            "service".to_string(),
            "language".to_string(),
            "0.1".to_string(),
            "1.0".to_string(),
        )
        .spawn_with_config(Config::default())
        .await
        .unwrap();

        let metrics = Metrics::new(&worker);

        assert!(!metrics.0.is_empty());

        assert!(metrics.get(API_REQUEST_STR).is_some());
        assert!(metrics.get(API_RESPONSES_STR).is_some());
        assert!(metrics.get(API_BYTES_STR).is_some());
        assert!(metrics.get(API_ERRORS_STR).is_some());
        assert!(metrics.get(CHUNKS_SENT_STR).is_some());
        assert!(metrics.get(CHUNKS_DROPPED_STR).is_some());
    }
}
