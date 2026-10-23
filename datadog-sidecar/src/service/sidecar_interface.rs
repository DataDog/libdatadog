// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::too_many_arguments)]

use crate::service::{
    ApplicationConfig, EvpTransportConfigWithIdentity, InstanceId, SerializedTracerHeaderTags,
    SessionConfig, SidecarAction,
};
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_ipc::platform::ShmHandle;
use libdd_live_debugger::sender::DebuggerType;
use libdd_telemetry::metrics::MetricContext;
use libdd_trace_utils::trace_utils::TracerGenericTags;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
pub enum DynamicInstrumentationConfigState {
    Enabled,
    Disabled,
    NotSet,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize)]
pub struct SidecarFlushOptions {
    pub traces_and_stats: bool,
    pub flag_evaluations: bool,
    pub telemetry: bool,
}

/// The `SidecarInterface` trait defines the necessary methods for the sidecar service.
///
/// A connection serves a single client thread, which processes one request at a time. All
/// session and application state is therefore bound to the connection: it is set by the
/// `set_*` methods (which the client replays after a reconnect) and implicitly applies to every
/// other message on the same connection.
#[libdd_ipc_macros::service]
pub trait SidecarInterface {
    /// Enqueues a list of actions for the application of the current request.
    ///
    /// # Arguments
    ///
    /// * `actions` - The action type being enqueued.
    async fn enqueue_actions(actions: Vec<SidecarAction>);

    /// Enqueues a list of actions for an explicitly given service and env, independently of the
    /// application of the current request.
    ///
    /// # Arguments
    ///
    /// * `service_name` - The service the actions are attributed to.
    /// * `env_name` - The env the actions are attributed to.
    /// * `actions` - The action type being enqueued.
    async fn enqueue_actions_for_service(
        service_name: String,
        env_name: String,
        actions: Vec<SidecarAction>,
    );

    /// Binds the identity and configuration to this connection. Must be the first message sent
    /// on a connection.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The session and runtime ID of the client.
    /// * `remote_config_notify_target` - The client's notification event (Windows only).
    /// * `config` - The configuration to be set.
    async fn set_connection_config(
        instance_id: InstanceId,
        #[cfg(windows)]
        #[SerializedHandle]
        remote_config_notify_target: Option<
            crate::service::remote_configs::RemoteConfigNotifyTarget,
        >,
        config: SessionConfig,
    );

    /// Updates the process tags of this connection.
    ///
    /// # Arguments
    ///
    /// * `process_tags` - The process tags.
    async fn set_process_tags(process_tags: Vec<Tag>);

    /// Records whether `DD_SERVICE` is currently set (per-request mutable; tracer should refresh
    /// on RINIT).
    async fn set_user_service_defined(is_defined: bool);

    /// Sets the application of the current request, or clears it (`None`) at request end.
    ///
    /// # Arguments
    ///
    /// * `application` - The metadata of the application.
    /// * `remote_config_generation` - The SHM reader generation last read by the client (0 if
    ///   unread, `u64::MAX` to never request a notification).
    async fn set_application(application: Option<ApplicationConfig>, remote_config_generation: u64);

    /// Registers a telemetry metric context on this connection.
    ///
    /// Registrations are connection-bound: tracked per connection, never dropped,
    /// and automatically replayed after a reconnect.
    ///
    /// # Arguments
    ///
    /// * `metric` - The metric context to register on this connection.
    async fn register_telemetry_metric(metric: MetricContext);

    /// Sends a trace via shared memory.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle to the shared memory.
    /// * `len` - The size of the shared memory data.
    /// * `headers` - The serialized headers from the tracer.
    async fn send_trace_v04_shm(
        #[SerializedHandle] handle: ShmHandle,
        len: usize,
        headers: SerializedTracerHeaderTags,
    );

    /// Sends a trace as bytes.
    ///
    /// # Arguments
    ///
    /// * `data` - The trace data serialized as bytes.
    /// * `headers` - The serialized headers from the tracer.
    async fn send_trace_v04_bytes(data: Vec<u8>, headers: SerializedTracerHeaderTags);

    /// Sends a V1-encoded trace via shared memory. The sidecar decodes the V1 `TracerPayload`,
    /// can inspect it, and re-encodes it as V1 msgpack on the way to the agent's
    /// `/v1.0/traces` endpoint. Use this when the SDK speaks V1 natively.
    ///
    /// The V1 payload already carries lang/version/tracer-version/container-id itself, so only
    /// the generic bool/int flags need to cross the IPC boundary, unlike `send_trace_v04_shm`
    /// which still needs the full `SerializedTracerHeaderTags`. `lang_interpreter`/`lang_vendor`
    /// have no equivalent in the V1 payload model though, so those two still cross separately.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle to the shared memory.
    /// * `len` - The size of the shared memory data.
    /// * `generic` - The generic tracer header flags (stats/top-level computed, dropped counts).
    /// * `lang_interpreter` - The tracer's language interpreter, absent from the V1 payload.
    /// * `lang_vendor` - The tracer's language interpreter vendor, absent from the V1 payload.
    async fn send_trace_v1_shm(
        #[SerializedHandle] handle: ShmHandle,
        len: usize,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    );

    /// Sends a V1-encoded trace as bytes. The sidecar decodes the V1 `TracerPayload`, can
    /// inspect it, and re-encodes it as V1 msgpack on the way to the agent's `/v1.0/traces`
    /// endpoint. Use this when the SDK speaks V1 natively.
    ///
    /// The V1 payload already carries lang/version/tracer-version/container-id itself, so only
    /// the generic bool/int flags need to cross the IPC boundary, unlike `send_trace_v04_bytes`
    /// which still needs the full `SerializedTracerHeaderTags`. `lang_interpreter`/`lang_vendor`
    /// have no equivalent in the V1 payload model though, so those two still cross separately.
    ///
    /// # Arguments
    ///
    /// * `data` - The V1 trace data serialized as bytes.
    /// * `generic` - The generic tracer header flags (stats/top-level computed, dropped counts).
    /// * `lang_interpreter` - The tracer's language interpreter, absent from the V1 payload.
    /// * `lang_vendor` - The tracer's language interpreter vendor, absent from the V1 payload.
    async fn send_trace_v1_bytes(
        data: Vec<u8>,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    );

    /// Transfers raw data to a live-debugger endpoint, on behalf of the current application.
    ///
    /// # Arguments
    /// * `handle` - The data to send.
    /// * `debugger_type` - Whether it's log or diagnostic data.
    async fn send_debugger_data_shm(
        #[SerializedHandle] handle: ShmHandle,
        debugger_type: DebuggerType,
    );

    /// Submits debugger diagnostics, on behalf of the current application.
    /// They are small and bounded in size, hence it's fine to send them without shm.
    /// Also, the sidecar server deserializes them to inspect and filter and avoid sending redundant
    /// diagnostics payloads.
    ///
    /// # Arguments
    /// * `diagnostics_payload` - The diagnostics data to send. (Sent as u8 json due to bincode
    ///   limitations)
    async fn send_debugger_diagnostics(diagnostics_payload: Vec<u8>);

    /// Acquire an exception hash rate limiter
    ///
    /// # Arguments
    /// * `exception_hash` - the ID
    /// * `granularity` - how much time needs to pass between two exceptions
    async fn acquire_exception_hash_rate_limiter(exception_hash: u64, granularity: Duration);

    /// Sends DogStatsD actions.
    ///
    /// # Arguments
    ///
    /// * `actions` - The DogStatsD actions to send.
    async fn send_dogstatsd_actions(actions: Vec<DogStatsDActionOwned>);

    /// Flushes outstanding traces/stats, flag evaluations, and/or telemetry, as specified by
    /// options.
    #[blocking]
    async fn flush(options: SidecarFlushOptions);

    /// Flush in the normal connection's packet order, then close the completion pipe.
    /// The raw signal worker can send this packet concurrently without disturbing normal replies.
    #[oneway]
    async fn flush_signal(
        options: SidecarFlushOptions,
        #[SerializedHandle] completion: libdd_ipc::platform::PlatformHandle<std::io::PipeWriter>,
    );

    /// Sets x-datadog-test-session-token on all requests of this connection.
    ///
    /// # Arguments
    ///
    /// * `token` - The session token.
    async fn set_test_session_token(token: String);

    /// IPC fallback: add a span directly to the sidecar's SHM concentrator for (env, version).
    /// Creates the concentrator if needed, retiring any stale segment.
    async fn add_span_to_concentrator(
        env: String,
        version: String,
        span: libdd_ipc::shm_stats::OwnedShmSpanInput,
    );

    /// IPC fallback: add a DSM stats point directly to the sidecar's SHM DSM table for
    /// (env, version, service, primary tag). Creates the table if needed, retiring any stale
    /// segment.
    async fn add_dsm_stats_point(
        env: String,
        version: String,
        service: String,
        primary_tag: String,
        point: libdd_ipc::shm_dsm::OwnedDsmStatsPoint,
    );

    /// Starts the AppSec backend if it has not already been initialized.
    ///
    /// Returns only after initialization finishes, so subsequent AppSec messages
    /// cannot overtake initialization.
    async fn ensure_appsec_started(log_file_path: Vec<u8>, log_level: String) -> bool;

    /// Forwards an AppSec message from the PHP extension to the helper's client of this
    /// connection.
    ///
    /// # Arguments
    ///
    /// * `data` - The message.
    /// * `client_init` - Whether the message is a client_init, which starts a new client.
    ///
    /// Returns the response bytes from the helper and a flag indicating whether the client is
    /// gone, and the extension has to start over with client_init.
    async fn send_appsec_message(
        #[ClientType(&'request [u8])] data: Vec<u8>,
        client_init: bool,
    ) -> (Vec<u8>, bool);

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

    /// Repurpose this connection as a crashtracker receiver.
    ///
    /// The connection must right after that start emitting crashtracker messages.
    async fn enter_crashtracker_receiver();

    /// Adds a batch of explicit per-target EVP transport updates to this session.
    ///
    /// This is separate from `SessionConfig` so consumers can opt in or update
    /// their target without replacing the session's other transports. Repeated
    /// identical configuration preserves routing and deduplication state.
    /// Targets absent from the batch are unchanged; this is not a full snapshot.
    ///
    /// Keep this method last: request variants cross the bincode IPC boundary,
    /// so appending preserves the ordinals of all existing messages.
    async fn add_session_evp_transports(configs: Vec<EvpTransportConfigWithIdentity>);
}

#[cfg(test)]
mod tests {
    use super::{SidecarInterfaceClientRequest, SidecarInterfaceRequest};
    use crate::service::{
        EvpProducerIdentity, EvpTransportConfig, EvpTransportConfigWithIdentity, EvpTransportMode,
    };
    use libdd_common::Endpoint;

    #[test]
    fn appsec_client_request_decodes_as_server_request() {
        let request = SidecarInterfaceClientRequest::SendAppsecMessage {
            data: b"payload",
            client_init: true,
        };

        let encoded = libdd_ipc::codec::encode(&request);
        let decoded: SidecarInterfaceRequest =
            libdd_ipc::codec::decode(&encoded).expect("client request should decode");

        match decoded {
            SidecarInterfaceRequest::SendAppsecMessage { data, client_init } => {
                assert_eq!(data, b"payload");
                assert!(client_init);
            }
            _ => panic!("decoded the wrong request variant"),
        }
    }

    #[test]
    fn evp_transport_config_decodes_across_the_ipc_boundary() {
        let agent_endpoint = Endpoint {
            url: "http://localhost:8126/".parse().unwrap(),
            ..Endpoint::default()
        };
        let direct_endpoint = Endpoint {
            url: "https://event-platform-intake.datadoghq.com/"
                .parse()
                .unwrap(),
            api_key: Some("test-api-key".into()),
            ..Endpoint::default()
        };

        let agentless_config = EvpTransportConfigWithIdentity::new(
            EvpTransportConfig {
                mode: EvpTransportMode::PreferLocalThenDirect,
                agent_endpoint,
                direct_endpoint: Some(direct_endpoint),
                intake_subdomain: "event-platform-intake".to_owned(),
            },
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap();
        let configs = vec![
            agentless_config,
            EvpTransportConfigWithIdentity::new(
                EvpTransportConfig::agent_only(Endpoint::default(), "errors-intake"),
                EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
            )
            .unwrap(),
        ];
        let encoded =
            libdd_ipc::codec::encode(&SidecarInterfaceClientRequest::AddSessionEvpTransports {
                configs: configs.clone(),
            });
        let decoded: SidecarInterfaceRequest = libdd_ipc::codec::decode(&encoded)
            .expect("identity-bearing Agentless config request should decode");
        match decoded {
            SidecarInterfaceRequest::AddSessionEvpTransports { configs: decoded } => {
                assert_eq!(decoded, configs);
            }
            _ => panic!("decoded the wrong Agentless config request variant"),
        }
    }
}
