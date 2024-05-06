// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::TemporarilyRetainedMapStats;
use crate::service::telemetry::enqueued_telemetry_stats::EnqueuedTelemetryStats;
use crate::service::tracing::trace_flusher::TraceFlusherStats;
pub use app_instance::AppInstance;
use ddtelemetry::worker::TelemetryWorkerStats;
use serde::{Deserialize, Serialize};

mod app_instance;
pub mod enqueued_telemetry_data;
pub mod enqueued_telemetry_stats;

#[derive(Serialize, Deserialize)]
pub(crate) struct SidecarStats {
    pub(crate) trace_flusher: TraceFlusherStats,
    pub(crate) sessions: u32,
    pub(crate) session_counter_size: u32,
    pub(crate) runtimes: u32,
    pub(crate) apps: u32,
    pub(crate) active_apps: u32,
    pub(crate) enqueued_apps: u32,
    pub(crate) enqueued_telemetry_data: EnqueuedTelemetryStats,
    pub(crate) telemetry_metrics_contexts: u32,
    pub(crate) telemetry_worker: TelemetryWorkerStats,
    pub(crate) telemetry_worker_errors: u32,
    pub(crate) log_writer: TemporarilyRetainedMapStats,
    pub(crate) log_filter: TemporarilyRetainedMapStats,
}
