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
use libdd_telemetry::metrics::MetricContext;
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerStats};
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::tracer_payload::decode_to_trace_chunks;
use libdd_trace_utils::tracer_payload::TraceEncoding;
use manual_future::ManualFutureCompleter;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
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
use crate::service::stats_flusher::{
    flush_all_stats_now, get_or_create_concentrator, stats_endpoint, ConcentratorKey,
    SpanConcentratorState, StatsConfig,
};
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
#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct ProcessHandle(pub winapi::um::winnt::HANDLE);

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
    /// Weak references to per-connection payload counters, for telemetry aggregation.
    pub(crate) connection_counters: Arc<Mutex<Vec<Weak<AtomicU64>>>>,
    /// All tracked agent infos per endpoint
    pub agent_infos: AgentInfos,
    /// All remote config handling
    remote_configs: RemoteConfigs,
    /// Diagnostics bookkeeper
    debugger_diagnostics_bookkeeper: Arc<DebuggerDiagnosticsBookkeeper>,
    /// Per-env&version SHM span concentrators (global across all sessions).
    pub(crate) span_concentrators: Arc<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
}

/// Per-connection handler wrapper that tracks sessions/instances for cleanup on disconnect.
struct ConnectionSidecarHandler {
    server: SidecarServer,
    /// Per-connection counter incremented on each received IPC message.
    submitted_payloads: Arc<AtomicU64>,
    session_id: std::sync::OnceLock<String>,
    instances: Mutex<std::collections::HashSet<InstanceId>>,
    /// All telemetry metric registrations received on this connection, keyed by metric name.
    /// Used to auto-register metrics in newly-created telemetry clients when a metric point
    /// for a previously registered metric arrives for a new (service, env) combination.
    metric_registrations: Mutex<HashMap<String, MetricContext>>,
}

impl ConnectionSidecarHandler {
    fn new(server: SidecarServer) -> Self {
        let submitted_payloads = Arc::new(AtomicU64::new(0));
        server
            .connection_counters
            .lock_or_panic()
            .push(Arc::downgrade(&submitted_payloads));
        Self {
            server,
            submitted_payloads,
            session_id: Default::default(),
            instances: Default::default(),
            metric_registrations: Default::default(),
        }
    }

    fn track_instance(&self, instance_id: &InstanceId) {
        self.instances.lock_or_panic().insert(instance_id.clone());
    }

    async fn cleanup(&self) {
        let instances: Vec<InstanceId> = self.instances.lock_or_panic().iter().cloned().collect();

        if let Some(session_id) = self.session_id.get() {
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
    /// * `conn`: The connection to the client.
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

    pub(crate) fn get_session(&self, session_id: &str) -> SessionInfo {
        let mut sessions = self.sessions.lock_or_panic();
        match sessions.get(session_id) {
            Some(session) => session.clone(),
            None => {
                let mut session = SessionInfo::default();
                session.session_id = session_id.to_string();
                info!("Initializing new session: {}", session_id);
                sessions.insert(session_id.to_string(), session.clone());
                session
            }
        }
    }

    fn get_runtime(&self, instance_id: &InstanceId) -> RuntimeInfo {
        let session = self.get_session(&instance_id.session_id);
        session.get_runtime(&instance_id.runtime_id)
    }

    async fn stop_session(&self, session_id: &str) {
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
        let process_handle = (*session.process_handle.lock_or_panic())?;
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
                .filter_map(|client| {
                    client
                        .client
                        .lock_or_panic()
                        .as_ref()
                        .and_then(|c| c.worker.stats().ok())
                })
                .collect::<Vec<_>>();

            let metric_counts = clients
                .values()
                .map(|client| {
                    client
                        .client
                        .lock_or_panic()
                        .as_ref()
                        .map_or(0, |c| c.telemetry_metrics.len() as u32)
                })
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
}

impl SidecarInterface for ConnectionSidecarHandler {
    fn recv_counter(&self) -> &AtomicU64 {
        &self.submitted_payloads
    }

    async fn enqueue_actions(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) {
        self.track_instance(&instance_id);
        let connection_metric_registrations = self.metric_registrations.lock_or_panic().clone();
        let session = self.server.get_session(&instance_id.session_id);
        let trace_config = session.get_trace_config();
        let runtime_metadata = RuntimeMetadata::new(
            trace_config.language.clone(),
            trace_config.language_version.clone(),
            trace_config.tracer_version.clone(),
        );

        let rt_info = self.server.get_runtime(&instance_id);
        let mut applications = rt_info.lock_applications();

        if let Entry::Occupied(entry) = applications.entry(queue_id) {
            let service = entry
                .get()
                .service_name
                .as_deref()
                .unwrap_or("unknown-service");
            let env = entry.get().env.as_deref().unwrap_or("none");

            let process_tags = session.process_tags.lock_or_panic().clone();

            // Pre-compute session config so both the primary and retry get_or_create calls
            // can use it without re-locking the session.
            let session_config = session
                .session_config
                .lock_or_panic()
                .as_ref()
                .cloned()
                .unwrap_or_else(|| {
                    warn!("Failed to get telemetry session config for {instance_id:?}");
                    Config::default()
                });

            // Get or create the telemetry client.  If we observe None under the lock it means
            // another thread called take() (Stop) in the narrow window between get_or_create
            // returning and us acquiring the lock — retry once to get a fresh client.
            let telemetry_mutex = self.server.telemetry_clients.get_or_create(
                service,
                env,
                &instance_id,
                &runtime_metadata,
                || session_config.clone(),
                process_tags.clone(),
            );
            let telemetry_mutex = if telemetry_mutex.lock_or_panic().is_none() {
                self.server.telemetry_clients.get_or_create(
                    service,
                    env,
                    &instance_id,
                    &runtime_metadata,
                    || session_config,
                    process_tags,
                )
            } else {
                telemetry_mutex
            };
            let mut telemetry_guard = telemetry_mutex.lock_or_panic();
            let Some(telemetry) = telemetry_guard.as_mut() else {
                // Extremely rare: the client was stopped between the two get_or_create calls.
                warn!("enqueue_actions: telemetry client stopped during retry for instance {instance_id:?}; dropping actions");
                return;
            };

            // Auto-register any metrics known to this connection but not yet registered
            // in this telemetry client (e.g., the client was just created for a new service/env).
            for action in &actions {
                if let SidecarAction::AddTelemetryMetricPoint((name, _, _)) = action {
                    if !telemetry.telemetry_metrics.contains_key(name) {
                        if let Some(metric) = connection_metric_registrations.get(name) {
                            telemetry.register_metric(metric.clone());
                        }
                    }
                }
            }

            let mut actions_to_process: Vec<SidecarAction> = vec![];
            let mut composer_paths_to_process = vec![];
            let mut buffered_info_changed = false;
            let mut remove_client = false;

            for action in actions {
                match action {
                    SidecarAction::Telemetry(TelemetryActions::AddIntegration(ref integration)) => {
                        if telemetry.shared.integrations.insert(integration.clone()) {
                            actions_to_process.push(action);
                            buffered_info_changed = true;
                        }
                    }
                    SidecarAction::PhpComposerTelemetryFile(path) => {
                        if telemetry.shared.composer_paths.insert(path.clone()) {
                            composer_paths_to_process.push(path);
                            buffered_info_changed = true;
                        }
                    }
                    SidecarAction::Telemetry(TelemetryActions::AddConfig(_)) => {
                        telemetry.shared.config_sent = true;
                        buffered_info_changed = true;
                        actions_to_process.push(action);
                    }
                    SidecarAction::Telemetry(TelemetryActions::AddEndpoint(_)) => {
                        telemetry.shared.last_endpoints_push = SystemTime::now();
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

            if buffered_info_changed {
                info!(
                    "Buffered telemetry info changed for instance {instance_id:?} and queue_id {queue_id:?}"
                );
                telemetry.write_shm_file();
            }

            // take() must happen INSIDE the spawned task, after process_actions completes,
            // so that a Config batch spawned before a Stop batch still finds Some when it
            // runs (the last_handle chain guarantees Stop runs after Config).
            let do_take = remove_client;

            if !actions_to_process.is_empty() {
                let telemetry_mutex_clone = telemetry_mutex.clone();
                let worker = telemetry.worker.clone();
                let last_handle = telemetry.handle.take();
                telemetry.handle = Some(tokio::spawn(async move {
                    if let Some(last_handle) = last_handle {
                        last_handle.await.ok();
                    };
                    let processed = {
                        let mut guard = telemetry_mutex_clone.lock_or_panic();
                        let processed = guard
                            .as_mut()
                            .map(|t| t.process_actions(actions_to_process))
                            .unwrap_or_default();
                        if do_take {
                            guard.take(); // drop client after Stop action is processed
                        }
                        processed
                    };
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

            // telemetry borrow ends after the last use of telemetry.handle above.
            // Remove from the map synchronously so new get_or_create calls get a fresh entry;
            // take() is deferred to the spawned task to avoid racing with in-flight tasks.
            if remove_client {
                self.server
                    .telemetry_clients
                    .remove_telemetry_client(service, env);
                info!("Removing telemetry client for instance {instance_id:?}");
            }
        } else {
            info!("No application found for instance {instance_id:?} and queue_id {queue_id:?}");
        }
    }

    async fn clear_queue_id(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
    ) {
        let rt_info = self.server.get_runtime(&instance_id);
        let mut applications = rt_info.lock_applications();
        if let Entry::Occupied(entry) = applications.entry(queue_id) {
            info!("Removing queue_id {queue_id:?} from instance {instance_id:?}");
            entry.remove();
        }
    }

    async fn register_telemetry_metric(&self, _peer: PeerCredentials, metric: MetricContext) {
        self.metric_registrations
            .lock_or_panic()
            .entry(metric.name.clone())
            .or_insert(metric);
    }

    async fn set_session_config(
        &self,
        peer: PeerCredentials,
        session_id: String,
        #[cfg(windows)] remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
        config: SessionConfig,
        is_fork: bool,
    ) {
        if self.session_id.set(session_id.clone()).is_ok() {
            let mut counter = self.server.session_counter.lock_or_panic();
            match counter.entry(session_id.clone()) {
                Entry::Occupied(mut e) => {
                    e.insert(e.get() + 1);
                }
                Entry::Vacant(e) => {
                    e.insert(1);
                }
            }
        }
        debug!("Set session config for {session_id} to {config:?}");

        let session = self.server.get_session(&session_id);
        session.pid.store(peer.pid as i32, Ordering::Relaxed);
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        {
            *session.remote_config_notify_function.lock().unwrap() = remote_config_notify_function;
            let handle = unsafe {
                winapi::um::processthreadsapi::OpenProcess(
                    winapi::um::winnt::PROCESS_ALL_ACCESS,
                    0,
                    peer.pid,
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
            let agent_info = self.server.agent_infos.query_for(config.endpoint.clone());
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
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: stats_endpoint(&config.endpoint).unwrap_or_else(|| config.endpoint.clone()),
            flush_interval: config.flush_interval,
            hostname: if config.hostname.is_empty() {
                sys_info::hostname().unwrap_or_default()
            } else {
                config.hostname.clone()
            },
            process_tags: config
                .process_tags
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(","),
            root_service: config.root_service.clone(),
            language: config.language.clone(),
            tracer_version: config.tracer_version.clone(),
        });

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
        self.server
            .trace_flusher
            .interval_ms
            .store(config.flush_interval.as_millis() as u64, Ordering::Relaxed);
        self.server
            .trace_flusher
            .min_force_flush_size_bytes
            .store(config.force_flush_size as u32, Ordering::Relaxed);
        self.server
            .trace_flusher
            .min_force_drop_size_bytes
            .store(config.force_drop_size as u32, Ordering::Relaxed);

        session.log_guard.lock_or_panic().replace((
            MULTI_LOG_FILTER.add(config.log_level),
            MULTI_LOG_WRITER.add(config.log_file),
        ));

        if let Some(completer) = self.server.self_telemetry_config.lock_or_panic().take() {
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

    async fn set_session_process_tags(&self, _peer: PeerCredentials, process_tags: Vec<Tag>) {
        let session_id = self
            .session_id
            .get()
            .map(|s| s.as_str())
            .unwrap_or_default();
        let session = self.server.get_session(session_id);
        *session.process_tags.lock_or_panic() = process_tags;
    }

    async fn shutdown_runtime(&self, _peer: PeerCredentials, instance_id: InstanceId) {
        let session = self.server.get_session(&instance_id.session_id);
        tokio::spawn(async move { session.shutdown_runtime(&instance_id.runtime_id).await });
    }

    async fn shutdown_session(&self, _peer: PeerCredentials) {
        let server = self.server.clone();
        let session_id = self.session_id.get().cloned().unwrap_or_default();
        tokio::spawn(async move { server.stop_session(&session_id).await });
    }

    async fn send_trace_v04_shm(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        handle: ShmHandle,
        _len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        self.track_instance(&instance_id);
        if let Some(endpoint) = self
            .server
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            let server = self.server.clone();
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

    async fn send_trace_v04_bytes(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) {
        self.track_instance(&instance_id);
        if let Some(endpoint) = self
            .server
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            let server = self.server.clone();
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

    async fn send_debugger_data_shm(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        handle: ShmHandle,
        debugger_type: DebuggerType,
    ) {
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
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

    async fn send_debugger_diagnostics(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    ) {
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
        #[allow(clippy::unwrap_used)]
        let payload = serde_json::from_slice(diagnostics_payload.as_slice()).unwrap();
        // We segregate RC by endpoint.
        // So we assume that runtime ids are unique per endpoint and we can safely filter globally.
        #[allow(clippy::unwrap_used)]
        if self
            .server
            .debugger_diagnostics_bookkeeper
            .add_payload(&payload)
        {
            session.send_debugger_data(
                DebuggerType::Diagnostics,
                &instance_id.runtime_id,
                queue_id,
                serde_json::to_vec(&vec![payload]).unwrap(),
            );
        }
    }

    async fn acquire_exception_hash_rate_limiter(
        &self,
        _peer: PeerCredentials,
        exception_hash: u64,
        granularity: Duration,
    ) {
        EXCEPTION_HASH_LIMITER
            .lock_or_panic()
            .add(exception_hash, granularity);
    }

    #[allow(clippy::too_many_arguments)]
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
        self.track_instance(&instance_id);
        debug!("Registered remote config metadata: instance {instance_id:?}, queue_id: {queue_id:?}, service: {service_name}, env: {env_name}, version: {app_version}");

        let session = self.server.get_session(&instance_id.session_id);
        let runtime_info = session.get_runtime(&instance_id.runtime_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        app.set_metadata(env_name, app_version, service_name, global_tags);
        let Some(notify_target) = self.server.get_notify_target(&session) else {
            return;
        };
        app.update_remote_config(
            &self.server.remote_configs,
            &session,
            instance_id,
            notify_target,
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
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
        let runtime_info = session.get_runtime(&instance_id.runtime_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        let Some(notify_target) = self.server.get_notify_target(&session) else {
            return;
        };
        app.update_remote_config(
            &self.server.remote_configs,
            &session,
            instance_id,
            notify_target,
            dynamic_instrumentation_state,
        );
    }

    async fn send_dogstatsd_actions(
        &self,
        _peer: PeerCredentials,
        instance_id: InstanceId,
        actions: Vec<DogStatsDActionOwned>,
    ) {
        self.track_instance(&instance_id);
        let server = self.server.clone();
        tokio::spawn(async move {
            server
                .get_session(&instance_id.session_id)
                .get_dogstatsd()
                .as_ref()
                .inspect(|f| f.send_owned(actions));
        });
    }

    async fn add_span_to_concentrator(
        &self,
        _peer: PeerCredentials,
        env: String,
        version: String,
        span: datadog_ipc::shm_stats::OwnedShmSpanInput,
    ) {
        let session_id = self.session_id.get().map(|s| s.as_str()).unwrap_or("");
        let session = self.server.get_session(session_id);
        // Lazily create the concentrator on first IPC span for this (env, version, service).
        if let Some(state) = get_or_create_concentrator(
            &self.server.span_concentrators,
            &env,
            &version,
            session_id,
            &session,
        ) {
            let mut peer_tag_buf = Vec::new();
            let input = span.as_shm_input(&mut peer_tag_buf);
            state.concentrator.add_span(&input);
        }
    }

    async fn flush_traces(&self, _peer: PeerCredentials) {
        let flusher = self.server.trace_flusher.clone();
        if let Err(e) = tokio::spawn(async move { flusher.flush().await }).await {
            error!("Failed flushing traces: {e:?}");
        }
        flush_all_stats_now(&self.server.span_concentrators).await;
    }

    async fn set_test_session_token(&self, _peer: PeerCredentials, token: String) {
        let session_id = self
            .session_id
            .get()
            .map(|s| s.as_str())
            .unwrap_or_default();
        let session = self.server.get_session(session_id);
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
        // Update the stats config so newly created concentrators carry the test token.
        session.modify_stats_config(|cfg| {
            cfg.endpoint.test_token = token.clone();
        });
        // TODO(APMSP-1377): the dogstatsd-client doesn't support test_session tokens yet
        // session.configure_dogstatsd(|cfg| {
        //     update_cfg(cfg.endpoint.take(), |e| cfg.set_endpoint(e), &token);
        // });
    }

    async fn ping(&self, _peer: PeerCredentials) {}

    async fn dump(&self, _peer: PeerCredentials) -> String {
        crate::dump::dump().await
    }

    async fn stats(&self, _peer: PeerCredentials) -> String {
        let stats = self.server.compute_stats().await;
        #[allow(clippy::expect_used)]
        simd_json::serde::to_string(&stats).expect("unable to serialize stats to string")
    }
}

// TODO: APMSP-1079 - Unit tests are sparse for the sidecar server. We should add more.
