// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::too_many_arguments)]

use crate::service::{
    InstanceId, QueueId, SerializedTracerHeaderTags, SessionConfig, SidecarAction,
};
use datadog_ipc::platform::ShmHandle;
use datadog_live_debugger::sender::DebuggerType;
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_telemetry::metrics::MetricContext;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
pub enum DynamicInstrumentationConfigState {
    Enabled,
    Disabled,
    NotSet,
}

/// The `SidecarInterface` trait defines the necessary methods for the sidecar service.
///
/// These methods include operations such as enqueueing actions, registering services, setting
/// session configurations, and sending traces.
#[datadog_ipc_macros::service]
pub trait SidecarInterface {
    /// Enqueues a list of actions to be performed.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the action in the queue.
    /// * `actions` - The action type being enqueued.
    async fn enqueue_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    );

    /// Sets the configuration for a session.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session.
    /// * `pid` - The pid of the sidecar client.
    /// * `config` - The configuration to be set.
    async fn set_session_config(
        session_id: String,
        #[cfg(windows)] remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
        config: SessionConfig,
        is_fork: bool,
    );

    /// Updates the process tags for an existing session.
    ///
    /// # Arguments
    ///
    /// * `process_tags` - The process tags.
    async fn set_session_process_tags(process_tags: Vec<Tag>);

    /// Removes the application entry for the given queue ID from the instance.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The queue ID to clear.
    async fn clear_queue_id(instance_id: InstanceId, queue_id: QueueId);

    /// Registers a telemetry metric context for a specific instance and queue.
    ///
    /// Registrations are connection-bound: tracked per connection, never dropped,
    /// and automatically replayed after a reconnect.
    ///
    /// # Arguments
    ///
    /// * `metric` - The metric context to register on this connection.
    async fn register_telemetry_metric(metric: MetricContext);

    /// Shuts down a runtime.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    async fn shutdown_runtime(instance_id: InstanceId);

    /// Shuts down a session.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session.
    async fn shutdown_session();

    /// Sends a trace via shared memory.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `handle` - The handle to the shared memory.
    /// * `len` - The size of the shared memory data.
    /// * `headers` - The serialized headers from the tracer.
    async fn send_trace_v04_shm(
        instance_id: InstanceId,
        #[SerializedHandle] handle: ShmHandle,
        len: usize,
        headers: SerializedTracerHeaderTags,
    );

    /// Sends a trace as bytes.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `data` - The trace data serialized as bytes.
    /// * `headers` - The serialized headers from the tracer.
    async fn send_trace_v04_bytes(
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    );

    /// Transfers raw data to a live-debugger endpoint.
    ///
    /// # Arguments
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the trace context.
    /// * `handle` - The data to send.
    /// * `debugger_type` - Whether it's log or diagnostic data.
    async fn send_debugger_data_shm(
        instance_id: InstanceId,
        queue_id: QueueId,
        #[SerializedHandle] handle: ShmHandle,
        debugger_type: DebuggerType,
    );

    /// Submits debugger diagnostics.
    /// They are small and bounded in size, hence it's fine to send them without shm.
    /// Also, the sidecar server deserializes them to inspect and filter and avoid sending redundant
    /// diagnostics payloads.
    ///
    /// # Arguments
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the trace context.
    /// * `diagnostics_payload` - The diagnostics data to send. (Sent as u8 json due to bincode
    ///   limitations)
    async fn send_debugger_diagnostics(
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    );

    /// Acquire an exception hash rate limiter
    ///
    /// # Arguments
    /// * `exception_hash` - the ID
    /// * `granularity` - how much time needs to pass between two exceptions
    async fn acquire_exception_hash_rate_limiter(exception_hash: u64, granularity: Duration);

    /// Sets contextual data
    ///
    /// # Arguments
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the trace context.
    /// * `service_name` - The name of the service.
    /// * `env_name` - The name of the environment.
    /// * `app_version` - The application version.
    /// * `global_tags` - Global tags which need to be propagated.
    /// * `dynamic_instrumentation_state` - Whether dynamic instrumentation is enabled, disabled or
    ///   not set.
    async fn set_universal_service_tags(
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    );

    /// Sets request state which does not directly affect the RC connection.
    ///
    /// # Arguments
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the trace context.
    /// * `dynamic_instrumentation_state` - Whether dynamic instrumentation is enabled, disabled or
    ///   not set.
    async fn set_request_config(
        instance_id: InstanceId,
        queue_id: QueueId,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    );

    /// Sends DogStatsD actions.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `actions` - The DogStatsD actions to send.
    async fn send_dogstatsd_actions(instance_id: InstanceId, actions: Vec<DogStatsDActionOwned>);

    /// Flushes any outstanding traces queued for sending.
    #[blocking]
    async fn flush_traces();

    /// Sets x-datadog-test-session-token on all requests for the given session.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session.
    /// * `token` - The session token.
    async fn set_test_session_token(token: String);

    /// IPC fallback: add a span directly to the sidecar's SHM concentrator for (env, version).
    ///
    /// Used when the PHP side cannot open the SHM concentrator yet (startup race: SHM is
    /// created by the sidecar after processing `set_universal_service_tags`, but span
    /// serialization may run before that message is processed).  Because the sidecar processes
    /// IPC messages sequentially and `set_universal_service_tags` is sent first (via the
    /// priority outbox), the concentrator is guaranteed to exist when this message is processed.
    async fn add_span_to_concentrator(
        env: String,
        version: String,
        span: datadog_ipc::shm_stats::OwnedShmSpanInput,
    );

    /// Sends a ping to the service.
    #[blocking]
    async fn ping();

    /// Dumps the current state of the service.
    ///
    /// # Returns
    ///
    /// A string representation of the current state of the service.
    async fn dump() -> String;

    /// Retrieves the current statistics of the service.
    ///
    /// # Returns
    ///
    /// A string representation of the current statistics of the service.
    async fn stats() -> String;
}
