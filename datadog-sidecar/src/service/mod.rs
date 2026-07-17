// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// imports for structs defined in this file
use crate::config;
pub use datadog_ffe::telemetry::evaluation_metrics::FfeEvaluationMetric;
pub use datadog_ffe::telemetry::exposures::{FfeExposure, FfeExposureBatch};
pub use datadog_ffe::telemetry::FfeTelemetryContext;
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
use libdd_remote_config::{RemoteConfigCapabilities, RemoteConfigProduct};
use libdd_telemetry::worker::TelemetryActions;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

// public types we want to bring up to top level of service:: scope
pub use instance_id::InstanceId;
pub use queue_id::QueueId;
pub use runtime_metadata::RuntimeMetadata;
pub use serialized_tracer_header_tags::SerializedTracerHeaderTags;

// public to crate types we want to bring up to top level of service:: scope
pub(crate) use sidecar_server::SidecarServer;

use runtime_info::RuntimeInfo;
use session_info::SessionInfo;
pub(crate) use sidecar_interface::SidecarInterface;

pub mod agent_info;
pub mod blocking;
mod debugger_diagnostics_bookkeeper;
pub mod exception_hash_rate_limiter;
pub(crate) mod ffe_exposures_flusher;
pub(crate) mod ffe_metrics_flusher;
mod instance_id;
mod queue_id;
mod remote_configs;
mod runtime_info;
mod runtime_metadata;
pub mod sender;
mod serialized_tracer_header_tags;
mod session_info;
pub mod sidecar_interface;
pub(crate) mod sidecar_server;
pub mod stats_flusher;
pub mod telemetry;
pub(crate) mod tracing;

#[cfg(windows)]
pub use remote_configs::RemoteConfigNotifyFunction;
pub use sidecar_interface::{DynamicInstrumentationConfigState, SidecarFlushOptions};
pub(crate) use telemetry::{init_telemetry_sender, telemetry_action_receiver_task};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionConfig {
    pub endpoint: Endpoint,
    pub dogstatsd_endpoint: Endpoint,
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
    pub flush_interval: Duration,
    pub remote_config_poll_interval: Duration,
    pub telemetry_heartbeat_interval: Duration,
    pub telemetry_extended_heartbeat_interval: Duration,
    pub force_flush_size: usize,
    pub force_drop_size: usize,
    pub retry_interval: Duration,
    pub log_level: String,
    pub log_file: config::LogMethod,
    pub remote_config_products: Vec<RemoteConfigProduct>,
    pub remote_config_capabilities: Vec<RemoteConfigCapabilities>,
    pub remote_config_enabled: bool,
    pub process_tags: Vec<Tag>,
    pub peer_tag_keys: Vec<String>,
    pub span_kinds_stats_computed: Vec<String>,
    /// Tracer-configured hostname (from `DD_HOSTNAME`).  Empty means "not configured".
    pub hostname: String,
    /// Process-level service name (from `DD_SERVICE`), used as the stats concentrator key.
    pub root_service: String,
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    /// Optional OTLP metrics intake endpoint.
    pub otlp_metrics_endpoint: Option<Endpoint>,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum SidecarAction {
    Telemetry(TelemetryActions),
    AddTelemetryMetricPoint((String, f64, Vec<Tag>)),
    PhpComposerTelemetryFile(PathBuf),
    /// Structured FFE exposures. The sidecar owns JSON serialization,
    /// cross-request deduplication, and EVP delivery.
    FfeExposureBatch(FfeExposureBatch),
    /// Structured FFE evaluation metrics. The sidecar owns OTLP/protobuf
    /// aggregation, serialization, and delivery. This action must be sent only
    /// by SDKs that explicitly opted into native FFE metric ownership.
    FfeEvaluationMetrics {
        context: FfeTelemetryContext,
        metrics: Vec<FfeEvaluationMetric>,
    },
}
