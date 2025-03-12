// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::lock_or_panic;
use ddcommon::tag::Tag;
use ddtelemetry::metrics::{ContextKey, MetricContext};
use ddtelemetry::worker::{TelemetryActions, TelemetryWorkerHandle};
use futures::future::{BoxFuture, Shared};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppInstance {
    pub(crate) telemetry: TelemetryWorkerHandle,
    pub(crate) telemetry_worker_shutdown: Shared<BoxFuture<'static, Option<()>>>,
    pub(crate) telemetry_metrics: Arc<Mutex<HashMap<String, ContextKey>>>,
}

impl AppInstance {
    /// Registers a new metric to the `AppInstance`.
    ///
    /// This method will add the metric to the `telemetry_metrics` map if it does not already exist.
    ///
    /// # Arguments
    ///
    /// * `metric` - The metric context to be registered.
    pub(crate) fn register_metric(&mut self, metric: MetricContext) {
        let mut metrics = lock_or_panic(&self.telemetry_metrics);
        if !metrics.contains_key(&metric.name) {
            metrics.insert(
                metric.name.clone(),
                self.telemetry.register_metric_context(
                    metric.name,
                    metric.tags,
                    metric.metric_type,
                    metric.common,
                    metric.namespace,
                ),
            );
        }
    }

    /// Converts the provided parameters into a `TelemetryActions::AddPoint` action.
    ///
    /// This method will look up the metric name in the `telemetry_metrics` map and use the
    /// corresponding `ContextKey` to create the `TelemetryActions::AddPoint` action.
    ///
    /// # Arguments
    ///
    /// * `(name, val, tags)` - A tuple containing the metric name, value, and tags.
    ///
    /// # Returns
    ///
    /// * `TelemetryActions` - The created `TelemetryActions::AddPoint` action.
    pub(crate) fn to_telemetry_point(
        &self,
        (name, val, tags): (String, f64, Vec<Tag>),
    ) -> TelemetryActions {
        #[allow(clippy::unwrap_used)]
        TelemetryActions::AddPoint((
            val,
            *lock_or_panic(&self.telemetry_metrics).get(&name).unwrap(),
            tags,
        ))
    }
}

// TODO: APMSP-1079 - Add unit tests for AppInstance
