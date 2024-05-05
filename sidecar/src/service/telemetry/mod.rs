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
pub struct SidecarStats {
    pub trace_flusher: TraceFlusherStats,
    pub sessions: u32,
    pub session_counter_size: u32,
    pub runtimes: u32,
    pub apps: u32,
    pub active_apps: u32,
    pub enqueued_apps: u32,
    pub enqueued_telemetry_data: EnqueuedTelemetryStats,
    pub telemetry_metrics_contexts: u32,
    pub telemetry_worker: TelemetryWorkerStats,
    pub telemetry_worker_errors: u32,
    pub log_writer: TemporarilyRetainedMapStats,
    pub log_filter: TemporarilyRetainedMapStats,
}
