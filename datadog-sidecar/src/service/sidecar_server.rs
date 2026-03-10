// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::{TemporarilyRetainedMapStats, MULTI_LOG_FILTER, MULTI_LOG_WRITER};
use crate::service::{
    sidecar_interface::serve_sidecar_interface_connection,
    telemetry::{TelemetryCachedClient, TelemetryCachedClientSet},
    tracing::TraceFlusher,
    DynamicInstrumentationConfigState, InstanceId, QueueId, RuntimeInfo, RuntimeMetadata,
    SerializedTracerHeaderTags, SessionConfig, SessionInfo, SidecarAction, SidecarInterface,
};
use datadog_ipc::platform::{FileBackedHandle, ShmHandle};
use datadog_ipc::{PeerCredentials, SeqpacketConn};
use libdd_common::{Endpoint, MutexExt};
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerStats};
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::tracer_payload::decode_to_trace_chunks;
use libdd_trace_utils::tracer_payload::TraceEncoding;
use manual_future::ManualFutureCompleter;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, trace, warn};

use serde::{Deserialize, Serialize};

use crate::config::get_product_endpoint;
use crate::service::agent_info::AgentInfos;
use crate::service::debugger_diagnostics_bookkeeper::{
    DebuggerDiagnosticsBookkeeper, DebuggerDiagnosticsBookkeeperStats,
};
use crate::service::exception_hash_rate_limiter::EXCEPTION_HASH_LIMITER;
use crate::service::remote_configs::{RemoteConfigNotifyTarget, RemoteConfigs};
use crate::service::tracing::trace_flusher::TraceFlusherStats;
use crate::tokio_util::run_or_spawn_shared;
use datadog_live_debugger::sender::{agent_info_supports_debugger_v2_endpoint, DebuggerType};
use datadog_remote_config::fetch::{ConfigInvariants, ConfigOptions, MultiTargetStats};
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::{new, DogStatsDActionOwned};
use libdd_telemetry::config::Config;
use libdd_tinybytes as tinybytes;
use libdd_trace_utils::tracer_header_tags::TracerHeaderTags;

/// A Windows process handle used for remote config notification.
///
/// Wraps a raw `HANDLE` value (from `OpenProcess`). The handle is intentionally not
/// closed on drop — it is valid for the lifetime of the session.
#[cfg(windows)]
#[derive(Clone)]
pub struct ProcessHandle(pub winapi::um::winnt::HANDLE);

#[cfg(windows)]
impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProcessHandle({:p})", self.0)
    }
}

#[cfg(windows)]
impl PartialEq for ProcessHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[cfg(windows)]
impl Eq for ProcessHandle {}

#[cfg(windows)]
impl std::hash::Hash for ProcessHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.0 as usize).hash(state);
    }
}

#[cfg(windows)]
unsafe impl Send for ProcessHandle {}

#[cfg(windows)]
unsafe impl Sync for ProcessHandle {}

#[derive(Debug, Serialize, Deserialize)]
pub struct SidecarStats {
    trace_flusher: TraceFlusherStats,
    sessions: u32,
    session_counter_size: u32,
    runtimes: u32,
    active_telemetry_clients: u32,
    active_apps: u32,
    remote_config_clients: u32,
    remote_configs: MultiTargetStats,
    debugger_diagnostics_bookkeeping: DebuggerDiagnosticsBookkeeperStats,
    telemetry_metrics_contexts: u32,
    telemetry_worker: TelemetryWorkerStats,
    telemetry_worker_errors: u32,
    log_writer: TemporarilyRetainedMapStats,
    log_filter: TemporarilyRetainedMapStats,
}

/// The `SidecarServer` struct represents a server that handles sidecar operations.
///
/// It maintains a list of active sessions and a counter for each session.
/// It also holds a reference to a `TraceFlusher` for sending trace data,
/// and a `Mutex` guarding an optional `ManualFutureCompleter` for telemetry configuration.
#[derive(Default, Clone)]
pub struct SidecarServer {
    /// An `Arc` wrapped `TraceFlusher` used for sending trace data.
    pub(crate) trace_flusher: Arc<TraceFlusher>,
    /// A `Mutex` guarded `HashMap` that stores active sessions.
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    /// A `Mutex` guarded `HashMap` that keeps a count of each session.
    session_counter: Arc<Mutex<HashMap<String, u32>>>,
    /// A `Mutex` guarded `HashMap` that stores the active telemetry clients.
    pub(crate) telemetry_clients: TelemetryCachedClientSet,
    /// A `Mutex` guarded optional `ManualFutureCompleter` for telemetry configuration.
    pub self_telemetry_config:
        Arc<Mutex<Option<ManualFutureCompleter<libdd_telemetry::config::Config>>>>,
    /// Keeps track of the number of submitted payloads.
    pub(crate) submitted_payloads: Arc<AtomicU64>,
    /// All tracked agent infos per endpoint
    pub agent_infos: AgentInfos,
    /// All remote config handling
    remote_configs: RemoteConfigs,
    /// Diagnostics bookkeeper
    debugger_diagnostics_bookkeeper: Arc<DebuggerDiagnosticsBookkeeper>,
}

/// Per-connection handler wrapper that tracks sessions/instances for cleanup on disconnect.
struct ConnectionSidecarHandler {
    server: SidecarServer,
    sessions: std::sync::Mutex<std::collections::HashSet<String>>,
    instances: std::sync::Mutex<std::collections::HashSet<InstanceId>>,
}

impl ConnectionSidecarHandler {
    fn new(server: SidecarServer) -> Self {
        Self {
            server,
            sessions: Default::default(),
            instances: Default::default(),
        }
    }

    fn track_session(&self, session_id: &str) {
        if self.sessions.lock_or_panic().insert(session_id.to_owned()) {
            let mut counter = self.server.session_counter.lock_or_panic();
            match counter.entry(session_id.to_owned()) {
                Entry::Occupied(mut e) => {
                    e.insert(e.get() + 1);
                }
                Entry::Vacant(e) => {
                    e.insert(1);
                }
            }
        }
    }

    fn track_instance(&self, instance_id: &InstanceId) {
        self.instances.lock_or_panic().insert(instance_id.clone());
    }

    async fn cleanup(&self) {
        let sessions: Vec<String> = self.sessions.lock_or_panic().iter().cloned().collect();
        let instances: Vec<InstanceId> = self.instances.lock_or_panic().iter().cloned().collect();

        for session_id in &sessions {
            let stop = {
                let mut counter = self.server.session_counter.lock_or_panic();
                if let Entry::Occupied(mut entry) = counter.entry(session_id.clone()) {
                    if entry.insert(entry.get() - 1) == 1 {
                        entry.remove();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if stop {
                self.server.stop_session(session_id).await;
            }
        }
        for instance_id in instances {
            let maybe_session = self
                .server
                .sessions
                .lock_or_panic()
                .get(&instance_id.session_id)
                .cloned();
            if let Some(session) = maybe_session {
                session.shutdown_runtime(&instance_id.runtime_id).await;
            }
        }
    }
}

impl SidecarServer {
    /// Accepts a new connection and starts processing requests.
    ///
    /// This function creates a per-connection `ConnectionSidecarHandler` and serves the connection,
    /// then runs cleanup when the connection closes.
    ///
    /// # Arguments
    ///
    /// * `conn`: A `SeqpacketConn` that represents the connection to the client.
    pub async fn accept_connection(self, conn: SeqpacketConn) {
        let handler = Arc::new(ConnectionSidecarHandler::new(self));
        let handler_for_cleanup = handler.clone();
        serve_sidecar_interface_connection(conn, handler).await;
        handler_for_cleanup.cleanup().await;
    }

    /// Returns the number of active sidecar sessions.
    ///
    /// # Returns
    ///
    /// * `usize`: The number of active sessions.
    pub fn active_session_count(&self) -> usize {
        self.session_counter.lock_or_panic().len()
    }

    pub(crate) fn get_session(&self, session_id: &String) -> SessionInfo {
        let mut sessions = self.sessions.lock_or_panic();
        match sessions.get(session_id) {
            Some(session) => session.clone(),
            None => {
                let mut session = SessionInfo::default();
                session.session_id.clone_from(session_id);
                info!("Initializing new session: {}", session_id);
                sessions.insert(session_id.clone(), session.clone());
                session
            }
        }
    }

    fn get_runtime(&self, instance_id: &InstanceId) -> RuntimeInfo {
        let session = self.get_session(&instance_id.session_id);
        session.get_runtime(&instance_id.runtime_id)
    }

    async fn stop_session(&self, session_id: &String) {
        let session = match self.sessions.lock_or_panic().remove(session_id) {
            Some(session) => session,
            None => return,
        };

        info!("Shutting down session: {}", session_id);
        session.shutdown().await;
        debug!("Successfully shut down session: {}", session_id);
    }

    fn send_trace_v04(
        &self,
        headers: &SerializedTracerHeaderTags,
        data: tinybytes::Bytes,
        target: &Endpoint,
    ) {
        let headers: TracerHeaderTags = match headers.try_into() {
            Ok(headers) => headers,
            Err(e) => {
                error!("Failed to convert SerializedTracerHeaderTags into TracerHeaderTags with error {:?}", e);
                return;
            }
        };

        debug!(
            "Received {} bytes of data for {:?} with headers {:?}",
            data.len(),
            target,
            headers
        );

        match decode_to_trace_chunks(data, TraceEncoding::V04) {
            Ok((payload, size)) => {
                trace!("Parsed the trace payload and enqueuing it for sending: {payload:?}");
                let data = SendData::new(
                    size,
                    payload.into_tracer_payload_collection(),
                    headers,
                    target,
                );
                self.trace_flusher.enqueue(data);
            }
            Err(e) => {
                error!(
                    "Failed to collect trace chunks from msgpack with error {:?}",
                    e
                )
            }
        }
    }

    #[cfg(windows)]
    #[allow(clippy::unwrap_used)]
    fn get_notify_target(&self, session: &SessionInfo) -> Option<RemoteConfigNotifyTarget> {
        let notify_function = *session.remote_config_notify_function.lock().unwrap();
        if notify_function.0.is_null() {
            return None;
        }
        let process_handle = session.process_handle.lock_or_panic().clone()?;
        Some(RemoteConfigNotifyTarget {
            process_handle,
            notify_function,
        })
    }

    #[cfg(unix)]
    fn get_notify_target(&self, session: &SessionInfo) -> Option<RemoteConfigNotifyTarget> {
        Some(RemoteConfigNotifyTarget {
            pid: session.pid.load(Ordering::Relaxed),
        })
    }

    pub async fn compute_stats(&self) -> SidecarStats {
        let (futures, metric_counts): (Vec<_>, Vec<_>) = {
            let clients = self.telemetry_clients.inner.lock_or_panic();

            let futures = clients
                .values()
                .filter_map(|client| client.client.lock_or_panic().worker.stats().ok())
                .collect::<Vec<_>>();

            let metric_counts = clients
                .values()
                .map(|client| client.client.lock_or_panic().telemetry_metrics.len() as u32)
                .collect::<Vec<_>>();

            (futures, metric_counts)
        };

        let telemetry_stats = futures::future::join_all(futures).await;
        let telemetry_stats_errors = telemetry_stats.iter().filter(|r| r.is_err()).count() as u32;
        let sessions = self.sessions.lock_or_panic();

        SidecarStats {
            trace_flusher: self.trace_flusher.stats(),
            sessions: sessions.len() as u32,
            session_counter_size: self.session_counter.lock_or_panic().len() as u32,
            runtimes: sessions
                .values()
                .map(|s| s.lock_runtimes().len() as u32)
                .sum(),
            active_telemetry_clients: self
                .telemetry_clients
                .inner
                .lock_or_panic()
                .values()
                .count() as u32,
            active_apps: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| r.lock_applications().len() as u32)
                        .sum::<u32>()
                })
                .sum(),
            remote_config_clients: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| {
                            r.lock_applications()
                                .values()
                                .filter_map(|a| a.remote_config_guard.as_ref())
                                .count() as u32
                        })
                        .sum::<u32>()
                })
                .sum(),
            remote_configs: self.remote_configs.stats(),
            debugger_diagnostics_bookkeeping: self.debugger_diagnostics_bookkeeper.stats(),
            telemetry_metrics_contexts: metric_counts.into_iter().sum(),
            telemetry_worker_errors: telemetry_stats_errors
                + telemetry_stats.iter().filter(|v| v.is_err()).count() as u32,
            telemetry_worker: telemetry_stats.into_iter().filter_map(|v| v.ok()).sum(),
            log_filter: MULTI_LOG_FILTER.stats(),
            log_writer: MULTI_LOG_WRITER.stats(),
        }
    }

    pub fn shutdown(&self) {
        self.remote_configs.shutdown();
    }

    fn enqueue_actions_impl(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) {
        let session = self.get_session(&instance_id.session_id);
        let trace_config = session.get_trace_config();
        let runtime_metadata = RuntimeMetadata::new(
            trace_config.language.clone(),
            trace_config.language_version.clone(),
            trace_config.tracer_version.clone(),
        );

        let rt_info = self.get_runtime(&instance_id);
        let mut applications = rt_info.lock_applications();

        if let Entry::Occupied(entry) = applications.entry(queue_id) {
            // Avoid materializing a telemetry client just to clear it
            if actions.len() == 1 && matches!(actions[0], SidecarAction::ClearQueueId) {
                info!("Removing queue_id {queue_id:?} from instance {instance_id:?}");
                entry.remove();
                return;
            }

            let service = entry
                .get()
                .service_name
                .as_deref()
                .unwrap_or("unknown-service");
            let env = entry.get().env.as_deref().unwrap_or("none");

            let process_tags = session.process_tags.lock_or_panic().clone();

            // Lock telemetry client
            let telemetry_mutex = self.telemetry_clients.get_or_create(
                service,
                env,
                &instance_id,
                &runtime_metadata,
                || {
                    session
                        .session_config
                        .lock_or_panic()
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(|| {
                            warn!("Failed to get telemetry session config for {instance_id:?}");
                            Config::default()
                        })
                },
                process_tags,
            );
            let mut telemetry = telemetry_mutex.lock_or_panic();

            let mut actions_to_process: Vec<SidecarAction> = vec![];
            let mut composer_paths_to_process = vec![];
            let mut buffered_info_changed = false;
            let mut remove_entry = false;
            let mut remove_client = false;

            for action in actions {
                match action {
                    SidecarAction::Telemetry(TelemetryActions::AddIntegration(ref integration)) => {
                        if telemetry.buffered_integrations.insert(integration.clone()) {
                            actions_to_process.push(action);
                            buffered_info_changed = true;
                        }
                    }
                    SidecarAction::PhpComposerTelemetryFile(path) => {
                        if telemetry.buffered_composer_paths.insert(path.clone()) {
                            composer_paths_to_process.push(path);
                            buffered_info_changed = true;
                        }
                    }
                    SidecarAction::Telemetry(TelemetryActions::AddConfig(_)) => {
                        telemetry.config_sent = true;
                        buffered_info_changed = true;
                        actions_to_process.push(action);
                    }
                    SidecarAction::ClearQueueId => {
                        remove_entry = true;
                    }
                    SidecarAction::Telemetry(TelemetryActions::AddEndpoint(_)) => {
                        telemetry.last_endpoints_push = SystemTime::now();
                        buffered_info_changed = true;
                        actions_to_process.push(action);
                    }
                    SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                        LifecycleAction::Stop,
                    )) => {
                        remove_client = true;
                        actions_to_process.push(action);
                    }
                    _ => {
                        actions_to_process.push(action);
                    }
                }
            }

            if !actions_to_process.is_empty() {
                let telemetry_mutex_clone = telemetry_mutex.clone();
                let worker = telemetry.worker.clone();
                let last_handle = telemetry.handle.take();
                telemetry.handle = Some(tokio::spawn(async move {
                    if let Some(last_handle) = last_handle {
                        last_handle.await.ok();
                    };
                    let processed = telemetry_mutex_clone
                        .lock_or_panic()
                        .process_actions(actions_to_process);
                    debug!("Sending Processed Actions :{processed:?}");
                    worker.send_msgs(processed).await.ok();
                }));
            }

            if !composer_paths_to_process.is_empty() {
                let worker = telemetry.worker.clone();
                let last_handle = telemetry.handle.take();
                telemetry.handle = Some(tokio::spawn(async move {
                    if let Some(last_handle) = last_handle {
                        last_handle.await.ok();
                    };
                    let composer_actions =
                        TelemetryCachedClient::process_composer_paths(composer_paths_to_process)
                            .await;
                    debug!("Sending Composer Paths :{composer_actions:?}");
                    worker.send_msgs(composer_actions).await.ok();
                }));
            }

            if buffered_info_changed {
                info!(
                    "Buffered telemetry info changed for instance {instance_id:?} and queue_id {queue_id:?}"
                );
                telemetry.write_shm_file();
            }

            if remove_client {
                info!("Removing telemetry client for instance {instance_id:?}");
                self.telemetry_clients.remove_telemetry_client(service, env);
            }

            if remove_entry {
                info!("Removing queue_id {queue_id:?} from instance {instance_id:?}");
                entry.remove();
            }
        } else {
            info!("No application found for instance {instance_id:?} and queue_id {queue_id:?}");
        }
    }

    async fn set_session_config_impl(
        &self,
        session_id: String,
        peer_pid: u32,
        #[cfg(windows)] remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
        config: SessionConfig,
        is_fork: bool,
    ) {
        debug!("Set session config for {session_id} to {config:?}");

        let session = self.get_session(&session_id);
        session.pid.store(peer_pid as i32, Ordering::Relaxed);
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        {
            *session.remote_config_notify_function.lock().unwrap() = remote_config_notify_function;
            let handle = unsafe {
                winapi::um::processthreadsapi::OpenProcess(
                    winapi::um::winnt::PROCESS_ALL_ACCESS,
                    0,
                    peer_pid,
                )
            };
            if !handle.is_null() {
                *session.process_handle.lock_or_panic() = Some(ProcessHandle(handle));
            }
        }
        *session.remote_config_enabled.lock_or_panic() = config.remote_config_enabled;
        *session.process_tags.lock_or_panic() = config.process_tags.clone();
        session.modify_telemetry_config(|cfg| {
            cfg.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
            let endpoint = get_product_endpoint(
                libdd_telemetry::config::PROD_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(endpoint).ok();
            cfg.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
        });
        session.modify_trace_config(|cfg| {
            let endpoint = get_product_endpoint(
                libdd_trace_utils::config_utils::PROD_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(endpoint).ok();
            cfg.language.clone_from(&config.language);
            cfg.language_version.clone_from(&config.language_version);
            cfg.tracer_version.clone_from(&config.tracer_version);
        });
        session.configure_dogstatsd(|dogstatsd| {
            let d = new(config.dogstatsd_endpoint.clone()).ok();
            *dogstatsd = d;
        });
        session.modify_debugger_config(|cfg| {
            let diagnostics_endpoint = get_product_endpoint(
                datadog_live_debugger::sender::PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(diagnostics_endpoint).ok();
        });
        if config.endpoint.api_key.is_none() {
            // no agent info if agentless
            let agent_info = self.agent_infos.query_for(config.endpoint.clone());
            let session_info = session.clone();
            run_or_spawn_shared(agent_info.get(), move |info| {
                if !agent_info_supports_debugger_v2_endpoint(info) {
                    session_info.modify_debugger_config(|cfg| {
                        cfg.downgrade_to_diagnostics_endpoint();
                    });
                }
            });
            *session.agent_infos.lock_or_panic() = Some(agent_info);
        }
        session.set_remote_config_invariants(ConfigOptions {
            invariants: ConfigInvariants {
                language: config.language,
                tracer_version: config.tracer_version,
                endpoint: config.endpoint,
            },
            products: config.remote_config_products,
            capabilities: config.remote_config_capabilities,
        });
        *session.remote_config_interval.lock_or_panic() = config.remote_config_poll_interval;
        self.trace_flusher
            .interval_ms
            .store(config.flush_interval.as_millis() as u64, Ordering::Relaxed);
        self.trace_flusher
            .min_force_flush_size_bytes
            .store(config.force_flush_size as u32, Ordering::Relaxed);
        self.trace_flusher
            .min_force_drop_size_bytes
            .store(config.force_drop_size as u32, Ordering::Relaxed);

        session.log_guard.lock_or_panic().replace((
            MULTI_LOG_FILTER.add(config.log_level),
            MULTI_LOG_WRITER.add(config.log_file),
        ));

        if let Some(completer) = self.self_telemetry_config.lock_or_panic().take() {
            #[allow(clippy::expect_used)]
            let config = session
                .session_config
                .lock_or_panic()
                .as_ref()
                .expect("Expected session_config to be Some(Config) but received None")
                .clone();
            tokio::spawn(async move {
                completer.complete(config).await;
            });
        }

        if !is_fork {
            session.shutdown_running_instances().await;
        }
    }

    fn set_session_process_tags_impl(&self, session_id: String, process_tags: Vec<Tag>) {
        let session = self.get_session(&session_id);
        *session.process_tags.lock_or_panic() = process_tags;
    }

    fn shutdown_runtime_impl(&self, instance_id: InstanceId) {
        let session = self.get_session(&instance_id.session_id);
        tokio::spawn(async move { session.shutdown_runtime(&instance_id.runtime_id).await });
    }

    fn shutdown_session_impl(&self, session_id: String) {
        let server = self.clone();
        tokio::spawn(async move { server.stop_session(&session_id).await });
    }

    fn send_trace_v04_shm_impl(
        &self,
        instance_id: InstanceId,
        handle: ShmHandle,
        _len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        if let Some(endpoint) = self
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            let server = self.clone();
            tokio::spawn(async move {
                match handle.map() {
                    Ok(mapped) => {
                        let bytes = tinybytes::Bytes::from(mapped);
                        server.send_trace_v04(&headers, bytes, &endpoint);
                    }
                    Err(e) => error!("Failed mapping shared trace data memory: {}", e),
                }
            });
        } else {
            warn!(
                "Received trace data ({handle:?}) for missing session {}",
                instance_id.session_id
            );
        }
    }

    fn send_trace_v04_bytes_impl(
        &self,
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) {
        if let Some(endpoint) = self
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            let server = self.clone();
            tokio::spawn(async move {
                let bytes = tinybytes::Bytes::from(data);
                server.send_trace_v04(&headers, bytes, &endpoint);
            });
        } else {
            warn!(
                "Received trace data for missing session {}",
                instance_id.session_id
            );
        }
    }

    fn send_debugger_data_shm_impl(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        handle: ShmHandle,
        debugger_type: DebuggerType,
    ) {
        let session = self.get_session(&instance_id.session_id);
        match handle.map() {
            Ok(mapped) => {
                session.send_debugger_data(
                    debugger_type,
                    &instance_id.runtime_id,
                    queue_id,
                    mapped,
                );
            }
            Err(e) => error!("Failed mapping shared debugger data memory: {}", e),
        }
    }

    fn send_debugger_diagnostics_impl(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    ) {
        let session = self.get_session(&instance_id.session_id);
        #[allow(clippy::unwrap_used)]
        let payload = serde_json::from_slice(diagnostics_payload.as_slice()).unwrap();
        // We segregate RC by endpoint.
        // So we assume that runtime ids are unique per endpoint and we can safely filter globally.
        #[allow(clippy::unwrap_used)]
        if self.debugger_diagnostics_bookkeeper.add_payload(&payload) {
            session.send_debugger_data(
                DebuggerType::Diagnostics,
                &instance_id.runtime_id,
                queue_id,
                serde_json::to_vec(&vec![payload]).unwrap(),
            );
        }
    }

    fn acquire_exception_hash_rate_limiter_impl(&self, exception_hash: u64, granularity: Duration) {
        EXCEPTION_HASH_LIMITER
            .lock_or_panic()
            .add(exception_hash, granularity);
    }

    #[allow(clippy::too_many_arguments)]
    fn set_universal_service_tags_impl(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        debug!("Registered remote config metadata: instance {instance_id:?}, queue_id: {queue_id:?}, service: {service_name}, env: {env_name}, version: {app_version}");

        let session = self.get_session(&instance_id.session_id);
        let runtime_info = session.get_runtime(&instance_id.runtime_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        app.set_metadata(env_name, app_version, service_name, global_tags);
        let Some(notify_target) = self.get_notify_target(&session) else {
            return;
        };
        app.update_remote_config(
            &self.remote_configs,
            &session,
            instance_id,
            notify_target,
            dynamic_instrumentation_state,
        );
    }

    fn set_request_config_impl(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        let session = self.get_session(&instance_id.session_id);
        let runtime_info = session.get_runtime(&instance_id.runtime_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        let Some(notify_target) = self.get_notify_target(&session) else {
            return;
        };
        app.update_remote_config(
            &self.remote_configs,
            &session,
            instance_id,
            notify_target,
            dynamic_instrumentation_state,
        );
    }

    fn send_dogstatsd_actions_impl(&self, instance_id: InstanceId, actions: Vec<DogStatsDActionOwned>) {
        let server = self.clone();
        tokio::spawn(async move {
            server
                .get_session(&instance_id.session_id)
                .get_dogstatsd()
                .as_ref()
                .inspect(|f| f.send_owned(actions));
        });
    }

    async fn flush_traces_impl(&self) {
        let flusher = self.trace_flusher.clone();
        if let Err(e) = tokio::spawn(async move { flusher.flush().await }).await {
            error!("Failed flushing traces: {e:?}");
        }
    }

    fn set_test_session_token_impl(&self, session_id: String, token: String) {
        let session = self.get_session(&session_id);
        let token = if token.is_empty() {
            None
        } else {
            Some(Cow::Owned(token))
        };
        debug!("Update test token of session {session_id} to {token:?}");
        session.modify_telemetry_config(|telemetry_cfg| {
            telemetry_cfg.set_endpoint_test_token(token.clone());
        });
        session.modify_trace_config(|trace_cfg| {
            trace_cfg.set_endpoint_test_token(token.clone());
        });
        // TODO(APMSP-1377): the dogstatsd-client doesn't support test_session tokens yet
        // session.configure_dogstatsd(|cfg| {
        //     update_cfg(cfg.endpoint.take(), |e| cfg.set_endpoint(e), &token);
        // });
    }

    async fn dump_impl(&self) -> String {
        crate::dump::dump().await
    }

    async fn stats_impl(&self) -> String {
        let stats = self.compute_stats().await;
        #[allow(clippy::expect_used)]
        simd_json::serde::to_string(&stats).expect("unable to serialize stats to string")
    }
}

impl SidecarInterface for ConnectionSidecarHandler {
    async fn enqueue_actions(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server.enqueue_actions_impl(instance_id, queue_id, actions);
    }

    async fn set_session_config(
        &self,
        peer: PeerCredentials,
        session_id: String,
        #[cfg(windows)] remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
        config: SessionConfig,
        is_fork: bool,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_session(&session_id);
        self.server
            .set_session_config_impl(
                session_id,
                peer.pid,
                #[cfg(windows)] remote_config_notify_function,
                config,
                is_fork,
            )
            .await;
    }

    async fn set_session_process_tags(
        &self,
        _peer: PeerCredentials,
        session_id: String,
        process_tags: String,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_session(&session_id);
        self.server
            .set_session_process_tags_impl(session_id, process_tags);
    }

    async fn shutdown_runtime(&self, _peer: PeerCredentials, instance_id: InstanceId) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server.shutdown_runtime_impl(instance_id);
    }

    async fn shutdown_session(&self, _peer: PeerCredentials, session_id: String) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_session(&session_id);
        self.server.shutdown_session_impl(session_id);
    }

    async fn send_trace_v04_shm(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        handle: ShmHandle,
        len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server
            .send_trace_v04_shm_impl(instance_id, handle, len, headers);
    }

    async fn send_trace_v04_bytes(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server
            .send_trace_v04_bytes_impl(instance_id, data, headers);
    }

    async fn send_debugger_data_shm(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        handle: ShmHandle,
        debugger_type: DebuggerType,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server
            .send_debugger_data_shm_impl(instance_id, queue_id, handle, debugger_type);
    }

    async fn send_debugger_diagnostics(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server
            .send_debugger_diagnostics_impl(instance_id, queue_id, diagnostics_payload);
    }

    async fn acquire_exception_hash_rate_limiter(
        &self,
        _peer: PeerCredentials,
        exception_hash: u64,
        granularity: Duration,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.server
            .acquire_exception_hash_rate_limiter_impl(exception_hash, granularity);
    }

    async fn set_universal_service_tags(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server.set_universal_service_tags_impl(
            instance_id,
            queue_id,
            service_name,
            env_name,
            app_version,
            global_tags,
            dynamic_instrumentation_state,
        );
    }

    async fn set_request_config(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server
            .set_request_config_impl(instance_id, queue_id, dynamic_instrumentation_state);
    }

    async fn send_dogstatsd_actions(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        actions: Vec<DogStatsDActionOwned>,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_instance(&instance_id);
        self.server.send_dogstatsd_actions_impl(instance_id, actions);
    }

    async fn flush_traces(&self, _peer: PeerCredentials) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.server.flush_traces_impl().await;
    }

    async fn set_test_session_token(
        &self,
        _peer: PeerCredentials,
        session_id: String,
        token: String,
    ) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.track_session(&session_id);
        self.server.set_test_session_token_impl(session_id, token);
    }

    async fn ping(&self, _peer: PeerCredentials) {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
    }

    async fn dump(&self, _peer: PeerCredentials) -> String {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.server.dump_impl().await
    }

    async fn stats(&self, _peer: PeerCredentials) -> String {
        self.server.submitted_payloads.fetch_add(1, Ordering::Relaxed);
        self.server.stats_impl().await
    }
}

// TODO: APMSP-1079 - Unit tests are sparse for the sidecar server. We should add more.
