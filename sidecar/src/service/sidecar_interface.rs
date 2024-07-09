// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::dogstatsd::DogStatsDAction;
use crate::service::{
    InstanceId, QueueId, RequestIdentification, RequestIdentifier, RuntimeMetadata,
    SerializedTracerHeaderTags, SessionConfig, SidecarAction,
};
use anyhow::Result;
use datadog_ipc::platform::ShmHandle;
use datadog_ipc::tarpc;

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

    /// Registers a service and flushes any queued actions.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the action in the queue.
    /// * `meta` - The metadata of the runtime.
    /// * `service_name` - The name of the service.
    /// * `env_name` - The name of the environment.
    async fn register_service_and_flush_queued_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        meta: RuntimeMetadata,
        service_name: String,
        env_name: String,
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
        pid: libc::pid_t,
        config: SessionConfig
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
    /// * `handle` - The data to send.
    async fn send_debugger_data_shm(
        instance_id: InstanceId,
        #[SerializedHandle] handle: ShmHandle,
    );

    /// Sets contextual data for the remote config client.
    ///
    /// # Arguments
    /// * `instance_id` - The ID of the instance.
    /// * `queue_id` - The unique identifier for the trace context.
    /// * `service_name` - The name of the service.
    /// * `env_name` - The name of the environment.
    /// * `app_version` - The application version.
    async fn set_remote_config_data(
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
    );

    /// Sends DogStatsD actions.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The ID of the instance.
    /// * `actions` - The DogStatsD actions to send.
    async fn send_dogstatsd_actions(instance_id: InstanceId, actions: Vec<DogStatsDAction>);

    /// Flushes any outstanding traces queued for sending.
    async fn flush_traces();

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
