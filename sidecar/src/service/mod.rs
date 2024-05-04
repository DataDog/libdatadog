// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config;
use crate::interface::EnqueuedTelemetryData;
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use ddtelemetry::metrics::MetricContext;
use ddtelemetry::worker::TelemetryActions;
use futures::future::Shared;
pub use instance_id::InstanceId;
use manual_future::ManualFuture;
pub use queue_id::QueueId;
pub use request_identification::{RequestIdentification, RequestIdentifier};
pub use runtime_info::{RuntimeInfo, SharedAppManualFut};
pub use runtime_metadata::RuntimeMetadata;
use serde::{Deserialize, Serialize};
pub use serialized_tracer_header_tags::SerializedTracerHeaderTags;
pub use session_info::SessionInfo;
pub use sidecar_interface::{
    SidecarInterface, SidecarInterfaceClient, SidecarInterfaceRequest, SidecarInterfaceResponse,
};
pub use sidecar_server::SidecarServer;
use std::path::PathBuf;
use std::time::Duration;

mod instance_id;
pub mod queue_id;
mod request_identification;
mod runtime_info;
mod runtime_metadata;
mod serialized_tracer_header_tags;
mod session_info;
mod sidecar_interface;
mod sidecar_server;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionConfig {
    pub endpoint: Endpoint,
    pub flush_interval: Duration,
    pub force_flush_size: usize,
    pub force_drop_size: usize,
    pub log_level: String,
    pub log_file: config::LogMethod,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum SidecarAction {
    Telemetry(TelemetryActions),
    RegisterTelemetryMetric(MetricContext),
    AddTelemetryMetricPoint((String, f64, Vec<Tag>)),
    PhpComposerTelemetryFile(PathBuf),
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum AppOrQueue {
    App(Shared<ManualFuture<(String, String)>>),
    Queue(EnqueuedTelemetryData),
}
