// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// imports for structs defined in this file
use crate::config;
use crate::service::telemetry::enqueued_telemetry_data::EnqueuedTelemetryData;
use datadog_remote_config::{RemoteConfigCapabilities, RemoteConfigProduct};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use ddtelemetry::metrics::MetricContext;
use ddtelemetry::worker::TelemetryActions;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

// public types we want to bring up to top level of service:: scope
pub use instance_id::InstanceId;
pub use queue_id::QueueId;
pub use runtime_metadata::RuntimeMetadata;
pub use serialized_tracer_header_tags::SerializedTracerHeaderTags;

// public to crate types we want to bring up to top level of service:: scope
pub(crate) use request_identification::{RequestIdentification, RequestIdentifier};
pub(crate) use sidecar_server::SidecarServer;

use runtime_info::RuntimeInfo;
use session_info::SessionInfo;
use sidecar_interface::{SidecarInterface, SidecarInterfaceRequest, SidecarInterfaceResponse};

pub mod blocking;
pub mod exception_hash_rate_limiter;
mod instance_id;
mod queue_id;
mod remote_configs;
mod request_identification;
mod runtime_info;
mod runtime_metadata;
mod serialized_tracer_header_tags;
mod session_info;
mod sidecar_interface;
pub(crate) mod sidecar_server;
mod telemetry;
pub(crate) mod tracing;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionConfig {
    pub endpoint: Endpoint,
    pub dogstatsd_endpoint: Endpoint,
    pub language: String,
    pub tracer_version: String,
    pub flush_interval: Duration,
    pub remote_config_poll_interval: Duration,
    pub telemetry_heartbeat_interval: Duration,
    pub exception_hash_rate_limiter_seconds: u32,
    pub force_flush_size: usize,
    pub force_drop_size: usize,
    pub log_level: String,
    pub log_file: config::LogMethod,
    pub remote_config_products: Vec<RemoteConfigProduct>,
    pub remote_config_capabilities: Vec<RemoteConfigCapabilities>,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum SidecarAction {
    Telemetry(TelemetryActions),
    RegisterTelemetryMetric(MetricContext),
    AddTelemetryMetricPoint((String, f64, Vec<Tag>)),
    PhpComposerTelemetryFile(PathBuf),
}
