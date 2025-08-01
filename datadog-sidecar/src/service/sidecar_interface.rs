// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::too_many_arguments)]

use crate::service::{
    InstanceId, QueueId, RequestIdentification, RequestIdentifier, SerializedTracerHeaderTags,
    SessionConfig, SidecarAction,
};
use anyhow::Result;
use datadog_ipc::platform::ShmHandle;
use datadog_ipc::tarpc;
use datadog_live_debugger::sender::DebuggerType;
use ddcommon::tag::Tag;
use dogstatsd_client::DogStatsDActionOwned;
use std::time::Duration;

// This is a bit weird, but depending on the OS we're interested in different things...
// and the macro expansion is not going to be happy with #[cfg()] instructions inside them.
// So we'll just define a type, a pid on unix, a function pointer on windows.
#[cfg(unix)]
type RemoteConfigNotifyTarget = libc::pid_t;
#[cfg(windows)]
type RemoteConfigNotifyTarget = crate::service::remote_configs::RemoteConfigNotifyFunction;

/// The `SidecarInterface` trait defines the necessary methods for the sidecar service.
///
/// These methods include operations such as enqueueing actions, registering services, setting
/// session configurations, and sending traces.
#[datadog_sidecar_macros::extract_request_id]
#[datadog_ipc_macros::impl_transfer_handles]
#[tarpc::service]
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
        remote_config_notify_target: RemoteConfigNotifyTarget,
        config: SessionConfig,
        is_fork: bool,
    );

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
    async fn shutdown_session(session_id: String);

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
    async fn set_universal_service_tags(
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
    );

    /// Sends DogStatsD actions.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `actions` - The DogStatsD actions to send.
    async fn send_dogstatsd_actions(instance_id: InstanceId, actions: Vec<DogStatsDActionOwned>);

    /// Flushes any outstanding traces queued for sending.
    async fn flush_traces();

    /// Sets x-datadog-test-session-token on all requests for the given session.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session.
    /// * `token` - The session token.
    async fn set_test_session_token(session_id: String, token: String);

    /// Sends a ping to the service.
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
