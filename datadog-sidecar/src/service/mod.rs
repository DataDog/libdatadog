// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// imports for structs defined in this file
use crate::config;
use libdd_common::Endpoint;
use libdd_common::tag::Tag;
pub use libdd_ffe::telemetry::FfeTelemetryContext;
pub use libdd_ffe::telemetry::evaluation_metrics::FfeEvaluationMetric;
pub use libdd_ffe::telemetry::exposures::{FfeExposure, FfeExposureBatch};
pub use libdd_ffe::telemetry::flagevaluation::{
    AllocationKey, ContextDD, ContextTruncationReason, EvalError, FfeFlagEvaluationBatch,
    FfeFlagEvaluationEvent, FieldOmissions, FlagEvalEventContext, FlagKey, MAX_CONTEXT_DEPTH,
    MAX_CONTEXT_FIELDS, MAX_FIELD_LENGTH, TargetingRuleKey, VariantKey, prune_context_json,
};
use libdd_remote_config::{RemoteConfigCapabilities, RemoteConfigProduct};
use libdd_telemetry::worker::TelemetryActions;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

// public types we want to bring up to top level of service:: scope
pub use connection_session::{ConnectionSession, ConnectionSessionHandle};
pub use instance_id::InstanceId;
pub use runtime_metadata::RuntimeMetadata;
pub use serialized_tracer_header_tags::SerializedTracerHeaderTags;

// public to crate types we want to bring up to top level of service:: scope
pub(crate) use sidecar_server::SidecarServer;

pub(crate) use sidecar_interface::SidecarInterface;

pub mod agent_info;
mod application;
pub mod blocking;
mod connection_session;
mod debugger_diagnostics_bookkeeper;
mod dogstatsd_pool;
pub(crate) mod evp_proxy;
pub mod exception_hash_rate_limiter;
pub(crate) mod ffe_evp_proxy;
pub(crate) mod ffe_exposures_flusher;
pub(crate) mod ffe_flagevaluation_flusher;
pub(crate) mod ffe_metrics_flusher;
mod instance_id;
mod remote_configs;
mod runtime_metadata;
pub mod sender;
mod serialized_tracer_header_tags;
pub mod sidecar_interface;
pub(crate) mod sidecar_server;
#[cfg(target_os = "linux")]
pub mod signal_flush;
pub mod stats_flusher;
pub mod telemetry;
pub(crate) mod tracing;

#[cfg(windows)]
pub use remote_configs::RemoteConfigNotifyTarget;
pub use sidecar_interface::{DynamicInstrumentationConfigState, SidecarFlushOptions};
pub use telemetry::InternalTelemetryActions;
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
    /// The service name the tracer resolves when none is configured, reported as the `svc.auto`
    /// process tag. Only set when process tags are propagated.
    pub default_service_name: Option<String>,
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

#[cfg(test)]
impl SessionConfig {
    /// An agentless configuration (so no agent info is fetched) with remote config disabled.
    pub(crate) fn for_test() -> Self {
        let endpoint = Endpoint {
            url: "datadoghq.com".parse().unwrap(),
            api_key: Some("test-api-key".into()),
            ..Endpoint::default()
        };
        SessionConfig {
            endpoint,
            dogstatsd_endpoint: Endpoint::default(),
            language: "php".to_owned(),
            language_version: "8.3".to_owned(),
            tracer_version: "1.0".to_owned(),
            flush_interval: Duration::from_secs(1),
            remote_config_poll_interval: Duration::from_secs(5),
            telemetry_heartbeat_interval: Duration::from_secs(60),
            telemetry_extended_heartbeat_interval: Duration::from_secs(3600),
            force_flush_size: 1 << 20,
            force_drop_size: 10 << 20,
            retry_interval: Duration::from_secs(1),
            log_level: "error".to_owned(),
            log_file: config::LogMethod::Disabled,
            remote_config_products: vec![],
            remote_config_capabilities: vec![],
            remote_config_enabled: false,
            process_tags: vec![],
            default_service_name: None,
            peer_tag_keys: vec![],
            span_kinds_stats_computed: vec![],
            hostname: "test-host".to_owned(),
            root_service: String::new(),
            root_session_id: None,
            parent_session_id: None,
            otlp_metrics_endpoint: None,
        }
    }
}

/// Metadata of the application served by the request currently processed on a connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplicationConfig {
    pub service_name: String,
    pub env_name: String,
    pub app_version: String,
    /// Global tags which need to be propagated.
    pub global_tags: Vec<Tag>,
    pub dynamic_instrumentation_state: DynamicInstrumentationConfigState,
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
    /// Structured FFE flag evaluation batch for the EVP flagevaluation track.
    /// The sidecar serializes and POSTs the batch to
    /// `/evp_proxy/v2/api/v2/flagevaluation` (fire-and-forget).
    ///
    /// Keep this appended after pre-existing variants: this enum crosses the
    /// bincode sidecar IPC boundary, so inserting a variant before existing
    /// variants changes their wire ordinals.
    FfeFlagEvaluationBatch(FfeFlagEvaluationBatch),
}
