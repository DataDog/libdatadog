// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::health_metrics::HealthMetric;
use ddcommon::tag::Tag;
use either::Either;
use libdd_dogstatsd_client::{Client, DogStatsDAction};
use tracing::debug;

/// Handles emission of health metrics to DogStatsD
#[derive(Debug)]
pub(crate) struct MetricsEmitter<'a> {
    dogstatsd: Option<&'a Client>,
    common_tags: &'a [Tag],
}

impl<'a> MetricsEmitter<'a> {
    /// Create a new MetricsEmitter
    pub(crate) fn new(dogstatsd: Option<&'a Client>, common_tags: &'a [Tag]) -> Self {
        Self {
            dogstatsd,
            common_tags,
        }
    }

    /// Emit a health metric to dogstatsd
    pub(crate) fn emit(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        let has_custom_tags = custom_tags.is_some();
        if let Some(flusher) = self.dogstatsd {
            let tags = match custom_tags {
                None => Either::Left(self.common_tags),
                Some(custom) => Either::Right(self.common_tags.iter().chain(custom)),
            };
            match metric {
                HealthMetric::Count(name, c) => {
                    debug!(
                        metric_name = name,
                        count = c,
                        has_custom_tags = has_custom_tags,
                        "Emitting health metric to dogstatsd"
                    );
                    flusher.send(vec![DogStatsDAction::Count(name, c, tags.into_iter())])
                }
                HealthMetric::Distribution(name, value) => {
                    debug!(
                        metric_name = name,
                        value = value,
                        has_custom_tags = has_custom_tags,
                        "Emitting distribution metric to dogstatsd"
                    );
                    flusher.send(vec![DogStatsDAction::Distribution(
                        name,
                        value as f64,
                        tags.into_iter(),
                    )])
                }
            }
        } else {
            debug!(
                metric = ?metric,
                "Skipping metric emission - dogstatsd client not configured"
            );
        }
    }
}

// Primary testing is done in the main TraceExporter module for now.
#[cfg(test)]
mod tests {
    use super::*;
    use ddcommon::tag;

    #[test]
    fn test_metrics_emitter_new() {
        let tags = vec![tag!("service", "test")];
        let emitter = MetricsEmitter::new(None, &tags);

        assert!(emitter.dogstatsd.is_none());
        assert_eq!(emitter.common_tags.len(), 1);
        assert_eq!(emitter.common_tags[0], tag!("service", "test"));
    }

    #[test]
    fn test_metrics_emitter_emit_no_client() {
        let tags = vec![tag!("env", "test")];
        let emitter = MetricsEmitter::new(None, &tags);

        // Should not panic when dogstatsd client is None
        emitter.emit(HealthMetric::Count("test.metric", 1), None);
        emitter.emit(
            HealthMetric::Count("test.metric", 5),
            Some(vec![&tag!("custom", "tag")]),
        );
        emitter.emit(HealthMetric::Distribution("test.distribution", 1024), None);
    }
}
