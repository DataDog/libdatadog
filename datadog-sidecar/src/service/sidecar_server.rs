// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::{TemporarilyRetainedMapStats, MULTI_LOG_FILTER, MULTI_LOG_WRITER};
use crate::service::{
    sidecar_interface::serve_sidecar_interface_connection,
    telemetry::{
        ApplicationTelemetryDispatch, DirectTelemetryLifecycleRegistry, DirectTelemetryRetirement,
        InitialTelemetryData, MetricsLogsClientSet, PendingApplicationAction,
        TelemetryActionSender, TelemetryCachedClient, TelemetryCachedClientSet,
        TelemetryWorkerMetadata,
    },
    tracing::TraceFlusher,
    DynamicInstrumentationConfigState, InstanceId, QueueId, RuntimeInfo, RuntimeMetadata,
    SerializedTracerHeaderTags, SessionConfig, SessionInfo, SidecarAction, SidecarFlushOptions,
    SidecarInterface,
};
use datadog_ipc::platform::{FileBackedHandle, ShmHandle};
use datadog_ipc::SeqpacketConn;
use libdd_common::{Endpoint, MutexExt};
use libdd_telemetry::metrics::MetricContext;
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerStats};
use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::tracer_payload::decode_to_trace_chunks;
use libdd_trace_utils::tracer_payload::TraceEncoding;
use manual_future::ManualFutureCompleter;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, trace, warn};

#[cfg(test)]
use crate::service::telemetry::{telemetry_action_receiver_task, InternalTelemetryActions};

use crate::config::get_product_endpoint;
use crate::service::agent_info::AgentInfos;
use crate::service::debugger_diagnostics_bookkeeper::{
    DebuggerDiagnosticsBookkeeper, DebuggerDiagnosticsBookkeeperStats,
};
use crate::service::exception_hash_rate_limiter::EXCEPTION_HASH_LIMITER;
use crate::service::ffe_exposures_flusher;
use crate::service::ffe_metrics_flusher;
use crate::service::remote_configs::{RemoteConfigNotifyTarget, RemoteConfigs};
use crate::service::stats_flusher::{
    flush_all_stats_now, get_or_create_concentrator, stats_endpoint, ConcentratorKey,
    SpanConcentratorState, StatsConfig,
};
use crate::service::tracing::trace_flusher::TraceFlusherStats;
use crate::tokio_util::run_or_spawn_shared;
use datadog_ipc::ipc_server::OwnedServerConn;
use datadog_live_debugger::sender::{agent_info_supports_debugger_v2_endpoint, DebuggerType};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::{DogStatsDActionOwned, DogStatsDClient};
use libdd_remote_config::fetch::{ConfigInvariants, ConfigOptions, MultiTargetStats};
use libdd_telemetry::config::{Config, TelemetryEndpoint};
use libdd_tinybytes as tinybytes;
use libdd_trace_utils::tracer_header_tags::TracerHeaderTags;
use serde::{Deserialize, Serialize};

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
    /// Telemetry clients for logs and metrics that are independent of application lifecycle.
    pub(crate) metrics_logs_clients: MetricsLogsClientSet,
    /// Installed together with the direct-action receiver so lifecycle cleanup can be ordered
    /// after every action already accepted by that receiver.
    direct_telemetry_sender: Arc<Mutex<Option<TelemetryActionSender>>>,
    pub(crate) direct_telemetry_lifecycles: DirectTelemetryLifecycleRegistry,
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
    /// HTTP client shared by FFE fire-and-forget forwarders for connection reuse.
    pub(crate) ffe_http_client: NativeCapabilities,
    /// Sidecar-owned exposure cache, shared across sessions/connections.
    pub(crate) ffe_exposure_deduplicator: ffe_exposures_flusher::ExposureDeduplicator,
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
    /// The connection this handler serves.
    connection: OwnedServerConn,
}

impl ConnectionSidecarHandler {
    fn new(server: SidecarServer, connection: OwnedServerConn) -> Self {
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
            connection,
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
            self.server.stop_runtime(&instance_id).await;
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
        let server_conn = match OwnedServerConn::new(conn) {
            Ok(c) => c,
            Err(e) => {
                error!("IPC serve: failed to set up connection: {e}");
                return;
            }
        };
        let handler = Arc::new(ConnectionSidecarHandler::new(self, server_conn));
        let handler_for_cleanup = handler.clone();
        serve_sidecar_interface_connection(handler).await;
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

    pub(crate) fn find_session(&self, session_id: &str) -> Option<SessionInfo> {
        self.sessions.lock_or_panic().get(session_id).cloned()
    }

    pub(crate) fn get_runtime(&self, instance_id: &InstanceId) -> RuntimeInfo {
        let session = self.get_session(&instance_id.session_id);
        let existing = session.find_runtime(&instance_id.runtime_id);
        let runtime = existing.unwrap_or_else(|| session.get_runtime(&instance_id.runtime_id));
        self.direct_telemetry_lifecycles.activate(instance_id);
        runtime
    }

    pub(crate) fn find_runtime(&self, instance_id: &InstanceId) -> Option<RuntimeInfo> {
        self.find_session(&instance_id.session_id)?
            .find_runtime(&instance_id.runtime_id)
    }

    pub(crate) fn install_direct_telemetry_sender(&self, sender: TelemetryActionSender) {
        self.direct_telemetry_sender.lock_or_panic().replace(sender);
    }

    async fn retire_direct_telemetry(&self, scope: DirectTelemetryRetirement) {
        let sender = self.direct_telemetry_sender.lock_or_panic().clone();
        if let Some(sender) = sender {
            match sender.retire(scope.clone()).await {
                Ok(()) => return,
                Err(error) => {
                    warn!(
                        "Failed to order direct telemetry cleanup through receiver; \
                         falling back to synchronous cleanup: {error}"
                    );
                }
            }
        }

        match scope {
            DirectTelemetryRetirement::Runtimes(instances) => {
                self.direct_telemetry_lifecycles.retire_runtimes(&instances);
                self.metrics_logs_clients.remove_runtimes(&instances);
            }
            DirectTelemetryRetirement::Session(session_id) => {
                self.direct_telemetry_lifecycles.retire_session(&session_id);
                self.metrics_logs_clients.remove_session(&session_id);
            }
        }
    }

    async fn stop_runtime(&self, instance_id: &InstanceId) {
        let runtime = self
            .find_session(&instance_id.session_id)
            .and_then(|session| session.take_runtime(&instance_id.runtime_id));
        self.retire_direct_telemetry(DirectTelemetryRetirement::Runtimes(HashSet::from([
            instance_id.clone(),
        ])))
        .await;
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
    }

    async fn stop_session(&self, session_id: &str) {
        let session = self.sessions.lock_or_panic().remove(session_id);
        self.telemetry_clients.remove_pending_session(session_id);
        self.retire_direct_telemetry(DirectTelemetryRetirement::Session(session_id.to_string()))
            .await;
        let Some(session) = session else {
            return;
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
        retry_interval: u64,
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
                let mut data = SendData::new(
                    size,
                    payload.into_tracer_payload_collection(),
                    headers,
                    target,
                );
                let strategy =
                    RetryStrategy::new(5, retry_interval, RetryBackoffType::Exponential, None);
                data.set_retry_strategy(strategy);
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
        let application_clients = self.telemetry_clients.clients();
        let active_telemetry_clients = application_clients
            .iter()
            .filter(|client| client.lock_or_panic().as_ref().is_some())
            .count() as u32;
        let cached_clients = application_clients
            .into_iter()
            .chain(self.metrics_logs_clients.clients())
            .collect::<Vec<_>>();
        let mut workers = Vec::with_capacity(cached_clients.len());
        let mut telemetry_metrics_contexts = 0;
        for client in cached_clients {
            if let Some(client) = client.lock_or_panic().as_ref() {
                workers.push(client.worker.clone());
                telemetry_metrics_contexts += client.telemetry_metrics.len() as u32;
            }
        }
        workers.extend(
            self.span_concentrators
                .lock_or_panic()
                .values()
                .filter_map(|state| state.telemetry.clone()),
        );

        let mut telemetry_stats_errors = 0;
        let futures = workers
            .into_iter()
            .filter_map(|worker| match worker.stats() {
                Ok(stats) => Some(stats),
                Err(_) => {
                    telemetry_stats_errors += 1;
                    None
                }
            });
        let telemetry_stats = futures::future::join_all(futures).await;
        telemetry_stats_errors += telemetry_stats
            .iter()
            .filter(|result| result.is_err())
            .count() as u32;
        let sessions = self.sessions.lock_or_panic();

        SidecarStats {
            trace_flusher: self.trace_flusher.stats(),
            sessions: sessions.len() as u32,
            session_counter_size: self.session_counter.lock_or_panic().len() as u32,
            runtimes: sessions
                .values()
                .map(|s| s.lock_runtimes().len() as u32)
                .sum(),
            active_telemetry_clients,
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
            telemetry_metrics_contexts,
            telemetry_worker_errors: telemetry_stats_errors,
            telemetry_worker: telemetry_stats.into_iter().filter_map(|v| v.ok()).sum(),
            log_filter: MULTI_LOG_FILTER.stats(),
            log_writer: MULTI_LOG_WRITER.stats(),
        }
    }

    pub fn shutdown(&self) {
        self.remote_configs.shutdown();
    }
}

#[derive(Debug)]
struct ScheduledApplicationActions {
    remove_client: bool,
    next_lifecycle_actions: Vec<PendingApplicationAction>,
    terminal_handle: Option<tokio::task::JoinHandle<()>>,
}

async fn await_terminal_handoff(handle: tokio::task::JoinHandle<()>) {
    if let Err(error) = handle.await {
        warn!("Terminal telemetry handoff failed: {error}");
    }
}

fn remove_application_client_after_handoff(
    clients: TelemetryCachedClientSet,
    service: String,
    env: String,
    client: Arc<Mutex<Option<TelemetryCachedClient>>>,
    terminal_handle: Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(handle) = terminal_handle {
        tokio::spawn(async move {
            await_terminal_handoff(handle).await;
            clients.remove_telemetry_client(&service, &env, &client);
        });
    } else {
        clients.remove_telemetry_client(&service, &env, &client);
    }
}

enum ScheduledApplicationWork {
    Direct(Vec<TelemetryActions>),
    Composer(Vec<PathBuf>),
}

fn push_direct_application_work(
    work: &mut Vec<ScheduledApplicationWork>,
    mut actions: Vec<TelemetryActions>,
) {
    if actions.is_empty() {
        return;
    }
    match work.last_mut() {
        Some(ScheduledApplicationWork::Direct(existing)) => existing.append(&mut actions),
        _ => work.push(ScheduledApplicationWork::Direct(actions)),
    }
}

fn schedule_application_actions(
    telemetry_mutex: &Arc<Mutex<Option<TelemetryCachedClient>>>,
    mut actions: Vec<PendingApplicationAction>,
    created: bool,
    instance_id: &InstanceId,
    queue_id: QueueId,
) -> Result<ScheduledApplicationActions, Vec<PendingApplicationAction>> {
    let mut telemetry_guard = telemetry_mutex.lock_or_panic();
    let Some(telemetry) = telemetry_guard.as_mut() else {
        warn!("enqueue_actions: telemetry client unavailable for instance {instance_id:?}");
        return Err(actions);
    };
    if telemetry.is_stopping() {
        warn!("enqueue_actions: telemetry client stopping for instance {instance_id:?}");
        return Err(actions);
    }

    let next_lifecycle_actions = actions
        .iter()
        .position(|pending_action| {
            matches!(
                pending_action.action,
                SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop))
            )
        })
        .filter(|stop_index| *stop_index + 1 < actions.len())
        .map(|stop_index| actions.split_off(stop_index + 1))
        .unwrap_or_default();

    for pending_action in &actions {
        if let SidecarAction::AddTelemetryMetricPoint((name, _, _)) = &pending_action.action {
            if !telemetry.telemetry_metrics.contains_key(name) {
                if let Some(metric) = &pending_action.metric_registration {
                    debug!(
                        "Registering pending telemetry metric {name} from instance {:?}",
                        pending_action.origin
                    );
                    telemetry.register_metric(metric.clone());
                }
            }
        }
    }

    let mut work = Vec::new();
    let mut buffered_info_changed = false;
    let mut remove_client = false;

    for pending_action in actions {
        let action = pending_action.action;
        if created && InitialTelemetryData::contains_seeded_action(&action) {
            continue;
        }
        match action {
            SidecarAction::Telemetry(TelemetryActions::AddIntegration(ref integration)) => {
                if telemetry.shared.integrations.insert(integration.clone()) {
                    push_direct_application_work(
                        &mut work,
                        telemetry.process_actions(vec![action]),
                    );
                    buffered_info_changed = true;
                }
            }
            SidecarAction::PhpComposerTelemetryFile(path) => {
                if telemetry.shared.composer_paths.insert(path.clone()) {
                    work.push(ScheduledApplicationWork::Composer(vec![path]));
                    buffered_info_changed = true;
                }
            }
            SidecarAction::Telemetry(TelemetryActions::AddConfig(_)) => {
                telemetry.shared.config_sent = true;
                buffered_info_changed = true;
                push_direct_application_work(&mut work, telemetry.process_actions(vec![action]));
            }
            SidecarAction::Telemetry(TelemetryActions::AddEndpoint(_)) => {
                telemetry.shared.last_endpoints_push = SystemTime::now();
                buffered_info_changed = true;
                push_direct_application_work(&mut work, telemetry.process_actions(vec![action]));
            }
            SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop)) => {
                telemetry.mark_stopping();
                remove_client = true;
                push_direct_application_work(&mut work, telemetry.process_actions(vec![action]));
            }
            _ => push_direct_application_work(&mut work, telemetry.process_actions(vec![action])),
        }
    }

    if buffered_info_changed {
        info!(
            "Buffered telemetry info changed for instance {instance_id:?} and queue_id {queue_id:?}"
        );
        telemetry.write_shm_file();
    } else if !remove_client {
        telemetry.retry_shm_file_if_due();
    }

    let (terminal_completion, terminal_worker_join) = if remove_client {
        let (completion, receiver) = tokio::sync::watch::channel(false);
        telemetry.terminal_handoff = Some(receiver);
        (Some(completion), telemetry.worker_join.take())
    } else {
        (None, None)
    };
    let mut terminal_handle = None;
    if !work.is_empty() {
        let worker = telemetry.worker.clone();
        let last_handle = telemetry.handle.take();
        let handle = tokio::spawn(async move {
            if let Some(last_handle) = last_handle {
                last_handle.await.ok();
            };
            for work_item in work {
                let processed = match work_item {
                    ScheduledApplicationWork::Direct(actions) => actions,
                    ScheduledApplicationWork::Composer(paths) => {
                        TelemetryCachedClient::process_composer_paths(paths).await
                    }
                };
                if !processed.is_empty() {
                    debug!("Sending processed application actions: {processed:?}");
                    if let Err(error) = worker.send_msgs(processed).await {
                        warn!("Failed to send application telemetry actions: {error}");
                    }
                }
            }
            drop(worker);
            if let Some(worker_join) = terminal_worker_join {
                if let Err(error) = worker_join.await {
                    warn!("Application telemetry worker shutdown failed: {error}");
                }
            }
            if let Some(completion) = terminal_completion {
                completion.send_replace(true);
            }
        });
        if remove_client {
            terminal_handle = Some(handle);
        } else if let Some(telemetry) = telemetry_guard.as_mut() {
            telemetry.handle = Some(handle);
        }
    }

    Ok(ScheduledApplicationActions {
        remove_client,
        next_lifecycle_actions,
        terminal_handle,
    })
}

impl SidecarInterface for ConnectionSidecarHandler {
    fn recv_counter(&self) -> &AtomicU64 {
        &self.submitted_payloads
    }

    fn connection(&self) -> &OwnedServerConn {
        &self.connection
    }

    async fn enter_crashtracker_receiver(&self) {
        #[cfg(unix)]
        crate::crashtracker::run_crashtracker_receiver(self.connection.async_conn()).await;
    }

    async fn enqueue_actions(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) {
        self.track_instance(&instance_id);
        let connection_metric_registrations = self.metric_registrations.lock_or_panic().clone();
        let session = self.server.get_session(&instance_id.session_id);
        let (runtime_metadata, trace_endpoint) = {
            let trace_config = session.get_trace_config();
            (
                RuntimeMetadata::new(
                    trace_config.language.clone(),
                    trace_config.language_version.clone(),
                    trace_config.tracer_version.clone(),
                ),
                trace_config.endpoint.clone(),
            )
        };

        let ffe_http_client = self.server.ffe_http_client.clone();
        let actions: Vec<SidecarAction> = actions
            .into_iter()
            .filter(|a| match a {
                SidecarAction::FfeExposureBatch(batch) => {
                    if let Some(base) = trace_endpoint.as_ref() {
                        if let Some(ep) = ffe_exposures_flusher::exposure_endpoint(base) {
                            let batch = batch.clone();
                            let client = ffe_http_client.clone();
                            let deduplicator = self.server.ffe_exposure_deduplicator.clone();
                            tokio::spawn(async move {
                                ffe_exposures_flusher::send_batch(
                                    &client,
                                    &ep,
                                    &deduplicator,
                                    batch,
                                )
                                .await;
                            });
                        } else {
                            debug!(
                                "ffe_exposures_flusher: could not derive endpoint, dropping batch"
                            );
                        }
                    } else {
                        debug!("ffe_exposures_flusher: no session endpoint, dropping batch");
                    }
                    false
                }
                SidecarAction::FfeEvaluationMetrics { context, metrics } => {
                    if let Some(ep) = session.get_otlp_metrics_endpoint().clone() {
                        let client = ffe_http_client.clone();
                        let context = context.clone();
                        let metrics = metrics.clone();
                        tokio::spawn(async move {
                            ffe_metrics_flusher::send_metrics(&client, &ep, context, metrics).await;
                        });
                    } else {
                        debug!("ffe_metrics_flusher: no configured endpoint, dropping batch");
                    }
                    false
                }
                _ => true,
            })
            .collect();

        if actions.is_empty() {
            return;
        }

        let (service, env) = {
            let rt_info = self.server.get_runtime(&instance_id);
            let applications = rt_info.lock_applications();
            let Some(application) = applications.get(&queue_id) else {
                info!(
                    "No application found for instance {instance_id:?} and queue_id {queue_id:?}"
                );
                return;
            };
            (
                application
                    .service_name
                    .clone()
                    .unwrap_or_else(|| "unknown-service".to_string()),
                application
                    .env
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            )
        };

        let process_tags = session.process_tags_with_svc_source();
        // Pre-compute session config so replacement get_or_create calls can use it
        // without re-locking the session.
        let session_config = session
            .session_config
            .lock_or_panic()
            .as_ref()
            .cloned()
            .unwrap_or_else(|| {
                warn!("Failed to get telemetry session config for {instance_id:?}");
                Config::default()
            });

        let mut pending_actions = PendingApplicationAction::from_actions(
            &instance_id,
            actions,
            &connection_metric_registrations,
        );
        loop {
            let mut initialized_terminal_handle = None;
            let dispatch = self.server.telemetry_clients.get_or_create_for_actions(
                TelemetryWorkerMetadata::new(
                    &service,
                    &env,
                    &instance_id,
                    &runtime_metadata,
                    process_tags.clone(),
                ),
                pending_actions,
                || session_config.clone(),
                |client, actions| match schedule_application_actions(
                    client,
                    actions,
                    true,
                    &instance_id,
                    queue_id,
                ) {
                    Ok(scheduled) => {
                        initialized_terminal_handle = scheduled.terminal_handle;
                        scheduled.remove_client
                    }
                    Err(returned) => {
                        warn!(
                            "New telemetry lifecycle rejected {} initialization actions",
                            returned.len()
                        );
                        false
                    }
                },
            );
            let (telemetry_mutex, actions, created, initialized_terminal) = match dispatch {
                ApplicationTelemetryDispatch::Pending => return,
                ApplicationTelemetryDispatch::Handoff {
                    mut completion,
                    actions,
                } => {
                    while !*completion.borrow() {
                        if completion.changed().await.is_err() {
                            warn!(
                                "Application telemetry handoff ended without completion for \
                                 {service:?}/{env:?}"
                            );
                            break;
                        }
                    }
                    pending_actions = actions;
                    continue;
                }
                ApplicationTelemetryDispatch::Ready {
                    client,
                    actions,
                    created,
                    remove_client,
                } => (client, actions, created, remove_client),
            };

            if initialized_terminal {
                info!("Removing terminal telemetry client for instance {instance_id:?}");
                if actions.is_empty() {
                    remove_application_client_after_handoff(
                        self.server.telemetry_clients.clone(),
                        service,
                        env,
                        telemetry_mutex,
                        initialized_terminal_handle,
                    );
                    return;
                }
                if let Some(handle) = initialized_terminal_handle {
                    await_terminal_handoff(handle).await;
                }
                self.server.telemetry_clients.remove_telemetry_client(
                    &service,
                    &env,
                    &telemetry_mutex,
                );
                pending_actions = actions;
                continue;
            }
            if actions.is_empty() {
                return;
            }

            match schedule_application_actions(
                &telemetry_mutex,
                actions,
                created,
                &instance_id,
                queue_id,
            ) {
                Ok(scheduled) => {
                    if scheduled.remove_client {
                        info!("Removing telemetry client for instance {instance_id:?}");
                    }
                    if scheduled.next_lifecycle_actions.is_empty() {
                        if scheduled.remove_client {
                            remove_application_client_after_handoff(
                                self.server.telemetry_clients.clone(),
                                service,
                                env,
                                telemetry_mutex,
                                scheduled.terminal_handle,
                            );
                        }
                        return;
                    }
                    if let Some(handle) = scheduled.terminal_handle {
                        await_terminal_handoff(handle).await;
                    }
                    if scheduled.remove_client {
                        self.server.telemetry_clients.remove_telemetry_client(
                            &service,
                            &env,
                            &telemetry_mutex,
                        );
                    }
                    pending_actions = scheduled.next_lifecycle_actions;
                }
                Err(returned) => {
                    // The selected client became unavailable or terminal before this batch
                    // acquired its mutex. Ownership remains here and is atomically offered to
                    // the next cache lifecycle.
                    pending_actions = returned;
                }
            }
        }
    }

    async fn clear_queue_id(&self, instance_id: InstanceId, queue_id: QueueId) {
        let rt_info = self.server.get_runtime(&instance_id);
        let mut applications = rt_info.lock_applications();
        if let Entry::Occupied(entry) = applications.entry(queue_id) {
            info!("Removing queue_id {queue_id:?} from instance {instance_id:?}");
            entry.remove();
        }
    }

    async fn register_telemetry_metric(&self, metric: MetricContext) {
        self.metric_registrations
            .lock_or_panic()
            .entry(metric.name.clone())
            .or_insert(metric);
    }

    async fn set_session_config(
        &self,
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
        session
            .pid
            .store(self.connection.peer().pid as i32, Ordering::Relaxed);
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        {
            *session.remote_config_notify_function.lock().unwrap() = remote_config_notify_function;
            let handle = unsafe {
                winapi::um::processthreadsapi::OpenProcess(
                    winapi::um::winnt::PROCESS_ALL_ACCESS,
                    0,
                    self.connection.peer().pid,
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
            cfg.telemetry_extended_heartbeat_interval =
                config.telemetry_extended_heartbeat_interval;
            let endpoint = get_product_endpoint(
                libdd_telemetry::config::PROD_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(TelemetryEndpoint {
                api_key: endpoint.api_key.as_deref().map(str::to_owned),
                test_token: endpoint.test_token.as_deref().map(str::to_owned),
                timeout_ms: endpoint.timeout_ms,
                use_system_resolver: endpoint.use_system_resolver,
                ..Default::default()
            })
            .ok();
            cfg.set_endpoint_uri(endpoint.url).ok();
            cfg.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
            cfg.telemetry_extended_heartbeat_interval =
                config.telemetry_extended_heartbeat_interval;
            cfg.session_id = Some(session_id.clone());
            cfg.parent_session_id = config.parent_session_id;
            cfg.root_session_id = config.root_session_id;
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
            cfg.retry_interval = config.retry_interval.as_millis() as u64;
        });
        session.modify_otlp_metrics_endpoint(|endpoint| {
            *endpoint = config.otlp_metrics_endpoint.clone();
        });
        session.configure_dogstatsd(|dogstatsd| {
            let d = DogStatsDClient::new(config.dogstatsd_endpoint.clone()).ok();
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
            process_tags: session
                .process_tags_with_svc_source()
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
            let config = session.session_config.lock_or_panic().as_ref().cloned();
            if let Some(config) = config {
                tokio::spawn(async move {
                    completer.complete(config).await;
                });
            } else {
                warn!("Session telemetry config unexpectedly unavailable for {session_id}");
            }
        }

        if !is_fork {
            let runtimes = session.take_running_instances();
            let instance_ids = runtimes
                .iter()
                .map(|runtime| runtime.instance_id.clone())
                .collect::<HashSet<_>>();
            self.server
                .retire_direct_telemetry(DirectTelemetryRetirement::Runtimes(instance_ids))
                .await;
            futures::future::join_all(
                runtimes
                    .into_iter()
                    .map(|runtime| async move { runtime.shutdown().await }),
            )
            .await;
        }
    }

    async fn set_session_process_tags(&self, process_tags: Vec<Tag>) {
        let session_id = self
            .session_id
            .get()
            .map(|s| s.as_str())
            .unwrap_or_default();
        let session = self.server.get_session(session_id);
        *session.process_tags.lock_or_panic() = process_tags;
        session.refresh_stats_process_tags();
    }

    async fn set_session_default_service_name(&self, name: Option<String>) {
        let session_id = self
            .session_id
            .get()
            .map(|s| s.as_str())
            .unwrap_or_default();
        let session = self.server.get_session(session_id);
        *session.auto_resolved_service_name.lock_or_panic() = name;
        session.refresh_stats_process_tags();
    }

    async fn set_session_user_service_defined(&self, is_defined: bool) {
        let session_id = self
            .session_id
            .get()
            .map(|s| s.as_str())
            .unwrap_or_default();
        let session = self.server.get_session(session_id);
        *session.user_service_defined.lock_or_panic() = is_defined;
        session.refresh_stats_process_tags();
    }

    async fn shutdown_runtime(&self, instance_id: InstanceId) {
        self.server.stop_runtime(&instance_id).await;
    }

    async fn shutdown_session(&self) {
        let session_id = self.session_id.get().cloned().unwrap_or_default();
        self.server.stop_session(&session_id).await;
    }

    async fn send_trace_v04_shm(
        &self,
        instance_id: InstanceId,
        handle: ShmHandle,
        _len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
        let trace_config = session.get_trace_config();
        if let Some(endpoint) = trace_config.endpoint.clone() {
            let server = self.server.clone();
            let retry_interval = trace_config.retry_interval;
            tokio::spawn(async move {
                match handle.map() {
                    Ok(mapped) => {
                        let bytes = tinybytes::Bytes::from(mapped);
                        server.send_trace_v04(&headers, bytes, &endpoint, retry_interval);
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
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) {
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
        let trace_config = session.get_trace_config();

        if let Some(endpoint) = trace_config.endpoint.clone() {
            let server = self.server.clone();
            let retry_interval = trace_config.retry_interval;
            tokio::spawn(async move {
                let bytes = tinybytes::Bytes::from(data);
                server.send_trace_v04(&headers, bytes, &endpoint, retry_interval);
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
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
        remote_config_generation: u64,
    ) {
        self.track_instance(&instance_id);
        debug!("Registered remote config metadata: instance {instance_id:?}, queue_id: {queue_id:?}, service: {service_name}, env: {env_name}, version: {app_version}");

        let session = self.server.get_session(&instance_id.session_id);
        let runtime_info = self.server.get_runtime(&instance_id);
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
            remote_config_generation,
            notify_target,
            dynamic_instrumentation_state,
        );
    }

    async fn set_request_config(
        &self,
        instance_id: InstanceId,
        queue_id: QueueId,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        self.track_instance(&instance_id);
        let session = self.server.get_session(&instance_id.session_id);
        let runtime_info = self.server.get_runtime(&instance_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        let Some(notify_target) = self.server.get_notify_target(&session) else {
            return;
        };
        app.update_remote_config(
            &self.server.remote_configs,
            &session,
            instance_id,
            !0u64, // no need for a notification here, just a config update
            notify_target,
            dynamic_instrumentation_state,
        );
    }

    async fn send_dogstatsd_actions(
        &self,
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
        env: String,
        version: String,
        span: datadog_ipc::shm_stats::OwnedShmSpanInput,
    ) {
        let session_id = self.session_id.get().map(|s| s.as_str()).unwrap_or("");
        let session = self.server.get_session(session_id);
        // Lazily create the concentrator on first IPC span for this (env, version, service).
        if let Some(state) =
            get_or_create_concentrator(&self.server.span_concentrators, &env, &version, &session)
        {
            let mut peer_tag_buf = Vec::new();
            let input = span.as_shm_input(&mut peer_tag_buf);
            state.concentrator.add_span(&input);
        }
    }

    async fn flush(&self, options: SidecarFlushOptions) {
        if options.traces_and_stats {
            let flusher = self.server.trace_flusher.clone();
            if let Err(e) = tokio::spawn(async move { flusher.flush().await }).await {
                error!("Failed flushing traces: {e:?}");
            }
            let stats_states = {
                let concentrators = self.server.span_concentrators.lock_or_panic();
                concentrators.values().cloned().collect::<Vec<_>>()
            };
            flush_all_stats_now(&stats_states).await;
            debug!("Finished executing flush() for traces and stats")
        }
        if options.telemetry {
            let mut workers = self.server.telemetry_clients.workers();
            workers.extend(self.server.metrics_logs_clients.workers());
            let stats_states = {
                let concentrators = self.server.span_concentrators.lock_or_panic();
                concentrators.values().cloned().collect::<Vec<_>>()
            };
            workers.extend(
                stats_states
                    .into_iter()
                    .filter_map(|state| state.telemetry.clone()),
            );
            futures::future::join_all(workers.into_iter().map(|worker| async move {
                let _ = worker
                    .send_msg(TelemetryActions::Lifecycle(
                        LifecycleAction::FlushMetricAggr,
                    ))
                    .await;
                let _ = worker
                    .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
                    .await;
                // now await completion
                let (tx, rx) = futures::channel::oneshot::channel();
                let _ = worker.send_msg(TelemetryActions::CollectStats(tx)).await;
                let _ = rx.await;
            }))
            .await;
        }
    }

    async fn set_test_session_token(&self, token: String) {
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
        session.modify_otlp_metrics_endpoint(|endpoint| {
            if let Some(endpoint) = endpoint {
                endpoint.test_token = token.clone();
            }
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

    async fn ping(&self) {}

    async fn dump(&self) -> String {
        crate::dump::dump().await
    }

    async fn stats(&self) -> String {
        let stats = self.server.compute_stats().await;
        #[allow(clippy::expect_used)]
        simd_json::serde::to_string(&stats).expect("unable to serialize stats to string")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeEvaluationMetric, FfeExposure, FfeExposureBatch, FfeTelemetryContext};
    use datadog_ipc::shm_stats::ShmSpanInput;
    use httpmock::{Method::POST, MockServer};
    use libdd_trace_stats::span_concentrator::FixedAggregationKey;
    use libdd_trace_utils::test_utils::create_send_data;
    use std::path::PathBuf;
    use std::time::Instant;
    use tokio::sync::Barrier;
    use tokio::time::{sleep, timeout, Duration as TokioDuration};

    /// Build a handler backed by a throwaway socketpair connection. These tests exercise
    /// `enqueue_actions`, which uses only the shared server state and never reads the connection,
    /// but the handler now requires one.
    fn test_handler(server: SidecarServer) -> ConnectionSidecarHandler {
        let (local, peer) = SeqpacketConn::socketpair().expect("socketpair");
        drop(peer);
        let conn = OwnedServerConn::new(local).expect("OwnedServerConn");
        ConnectionSidecarHandler::new(server, conn)
    }

    fn test_session_config(endpoint: Endpoint, root_service: &str) -> SessionConfig {
        SessionConfig {
            dogstatsd_endpoint: endpoint.clone(),
            endpoint,
            language: "php".to_string(),
            language_version: "8.3".to_string(),
            tracer_version: "test".to_string(),
            flush_interval: Duration::from_secs(60),
            remote_config_poll_interval: Duration::from_secs(60),
            telemetry_heartbeat_interval: Duration::from_secs(60),
            telemetry_extended_heartbeat_interval: Duration::from_secs(3600),
            force_flush_size: 0,
            force_drop_size: 0,
            retry_interval: Duration::from_millis(10),
            log_level: "off".to_string(),
            log_file: crate::config::LogMethod::Disabled,
            remote_config_products: Vec::new(),
            remote_config_capabilities: Vec::new(),
            remote_config_enabled: false,
            process_tags: Vec::new(),
            peer_tag_keys: Vec::new(),
            span_kinds_stats_computed: Vec::new(),
            hostname: String::new(),
            root_service: root_service.to_string(),
            root_session_id: None,
            parent_session_id: None,
            otlp_metrics_endpoint: None,
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn shutdown_removes_runtime_owned_telemetry() {
        const SERVICE: &str = "cleanup-service";
        const ENV: &str = "test";
        const METRIC: &str = "cleanup.metric";

        let server = SidecarServer::default();
        let handler = test_handler(server.clone());
        handler
            .session_id
            .set("session".to_string())
            .expect("test handler session should be unset");
        let runtime_a = InstanceId::new("session", "runtime-a");
        let runtime_b = InstanceId::new("session", "runtime-b");
        let other_runtime = InstanceId::new("other-session", "runtime");
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");

        for instance_id in [&runtime_a, &runtime_b, &other_runtime] {
            server.metrics_logs_clients.get_or_create_metrics_logs(
                SERVICE,
                ENV,
                instance_id,
                &runtime_metadata,
                Config::default,
                Vec::new(),
            );
        }
        assert!(server.metrics_logs_clients.register_metric(
            &runtime_a,
            SERVICE,
            ENV,
            MetricContext {
                name: METRIC.to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            },
        ));
        assert!(server.metrics_logs_clients.register_metric(
            &other_runtime,
            SERVICE,
            ENV,
            MetricContext {
                name: "other.cleanup.metric".to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            },
        ));
        handler.shutdown_runtime(runtime_a.clone()).await;
        timeout(TokioDuration::from_secs(1), async {
            while server
                .metrics_logs_clients
                .get_existing_metrics_logs(&runtime_a, SERVICE, ENV)
                .is_some()
            {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("runtime telemetry cleanup should complete");
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_a, SERVICE, ENV)
            .is_none());
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_b, SERVICE, ENV)
            .is_some());
        server.stop_runtime(&runtime_a).await;
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_a, SERVICE, ENV)
            .is_none());
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_b, SERVICE, ENV)
            .is_some());
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&other_runtime, SERVICE, ENV)
            .is_some());
        assert!(!server
            .metrics_logs_clients
            .registered_metrics(&other_runtime, SERVICE, ENV)
            .is_empty());

        handler.shutdown_session().await;
        timeout(TokioDuration::from_secs(1), async {
            while server
                .metrics_logs_clients
                .get_existing_metrics_logs(&runtime_b, SERVICE, ENV)
                .is_some()
                || !server
                    .metrics_logs_clients
                    .registered_metrics(&runtime_b, SERVICE, ENV)
                    .is_empty()
            {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("session telemetry cleanup should complete");
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_b, SERVICE, ENV)
            .is_none());
        assert!(server
            .metrics_logs_clients
            .registered_metrics(&runtime_b, SERVICE, ENV)
            .is_empty());
        assert!(server.sessions.lock_or_panic().get("session").is_none());
        server.stop_session("session").await;
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&runtime_b, SERVICE, ENV)
            .is_none());
        assert!(server
            .metrics_logs_clients
            .registered_metrics(&runtime_b, SERVICE, ENV)
            .is_empty());
        assert!(server.sessions.lock_or_panic().get("session").is_none());
        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&other_runtime, SERVICE, ENV)
            .is_some());
        assert!(!server
            .metrics_logs_clients
            .registered_metrics(&other_runtime, SERVICE, ENV)
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn runtime_cleanup_is_terminal_relative_to_direct_action_receiver() {
        const SERVICE: &str = "receiver-runtime-cleanup";
        const ENV: &str = "test";

        let hook = crate::service::telemetry::MetricRegistrationSnapshotHook::new();
        let server = SidecarServer {
            metrics_logs_clients: MetricsLogsClientSet::with_registration_snapshot_hook(
                hook.clone(),
            ),
            ..Default::default()
        };
        let (sender, receiver) = crate::service::telemetry::direct_telemetry_channel(&server);
        server.install_direct_telemetry_sender(sender.clone());
        let receiver_task = tokio::spawn(telemetry_action_receiver_task(server.clone(), receiver));

        let instance = InstanceId::new("session", "runtime");
        let session = server.get_session(&instance.session_id);
        session.get_runtime(&instance.runtime_id);
        *session.session_config.lock_or_panic() = Some(Config::default());

        sender
            .send_actions(InternalTelemetryActions {
                instance_id: instance.clone(),
                service_name: SERVICE.to_string(),
                env_name: ENV.to_string(),
                actions: vec![
                    crate::service::telemetry::InternalTelemetryAction::TelemetryAction(
                        TelemetryActions::AddLog((
                            libdd_telemetry::worker::LogIdentifier { identifier: 1 },
                            libdd_telemetry::data::Log {
                                message: "in flight".to_string(),
                                level: libdd_telemetry::data::LogLevel::Debug,
                                count: 1,
                                stack_trace: None,
                                tags: String::new(),
                                is_sensitive: false,
                                is_crash: false,
                            },
                        )),
                    ),
                ],
            })
            .await
            .expect("queue in-flight direct action");
        hook.wait_until_snapshot();

        let cleanup_server = server.clone();
        let cleanup_instance = instance.clone();
        let cleanup =
            tokio::spawn(async move { cleanup_server.stop_runtime(&cleanup_instance).await });
        hook.resume_creation();
        cleanup.await.expect("runtime cleanup task");

        sender
            .send_actions(InternalTelemetryActions {
                instance_id: instance.clone(),
                service_name: SERVICE.to_string(),
                env_name: ENV.to_string(),
                actions: vec![
                    crate::service::telemetry::InternalTelemetryAction::TelemetryAction(
                        TelemetryActions::Lifecycle(LifecycleAction::FlushData),
                    ),
                ],
            })
            .await
            .expect("queue late direct action");
        sender.barrier().await.expect("receiver barrier");

        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&instance, SERVICE, ENV)
            .is_none());
        assert!(session.lock_runtimes().get(&instance.runtime_id).is_none());
        receiver_task.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn session_cleanup_is_terminal_relative_to_direct_action_receiver() {
        const SERVICE: &str = "receiver-session-cleanup";
        const ENV: &str = "test";
        const METRIC: &str = "receiver.session.metric";

        let hook = crate::service::telemetry::MetricRegistrationSnapshotHook::new();
        let server = SidecarServer {
            metrics_logs_clients: MetricsLogsClientSet::with_registration_snapshot_hook(
                hook.clone(),
            ),
            ..Default::default()
        };
        let (sender, receiver) = crate::service::telemetry::direct_telemetry_channel(&server);
        server.install_direct_telemetry_sender(sender.clone());
        let receiver_task = tokio::spawn(telemetry_action_receiver_task(server.clone(), receiver));

        let instance = InstanceId::new("session", "runtime");
        let session = server.get_session(&instance.session_id);
        session.get_runtime(&instance.runtime_id);
        *session.session_config.lock_or_panic() = Some(Config::default());

        sender
            .send_actions(InternalTelemetryActions {
                instance_id: instance.clone(),
                service_name: SERVICE.to_string(),
                env_name: ENV.to_string(),
                actions: vec![
                    crate::service::telemetry::InternalTelemetryAction::RegisterTelemetryMetric(
                        MetricContext {
                            name: METRIC.to_string(),
                            tags: Vec::new(),
                            metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                            common: true,
                            namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
                        },
                    ),
                    crate::service::telemetry::InternalTelemetryAction::AddMetricPoint((
                        1.0,
                        METRIC.to_string(),
                        Vec::new(),
                    )),
                ],
            })
            .await
            .expect("queue in-flight metric registration");
        hook.wait_until_snapshot();

        let cleanup_server = server.clone();
        let cleanup = tokio::spawn(async move { cleanup_server.stop_session("session").await });
        hook.resume_creation();
        cleanup.await.expect("session cleanup task");

        sender
            .send_actions(InternalTelemetryActions {
                instance_id: instance.clone(),
                service_name: SERVICE.to_string(),
                env_name: ENV.to_string(),
                actions: vec![
                    crate::service::telemetry::InternalTelemetryAction::RegisterTelemetryMetric(
                        MetricContext {
                            name: "late.metric".to_string(),
                            tags: Vec::new(),
                            metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                            common: true,
                            namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
                        },
                    ),
                ],
            })
            .await
            .expect("queue late session action");
        sender.barrier().await.expect("receiver barrier");

        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&instance, SERVICE, ENV)
            .is_none());
        assert!(server
            .metrics_logs_clients
            .registered_metrics(&instance, SERVICE, ENV)
            .is_empty());
        assert!(server
            .sessions
            .lock_or_panic()
            .get(&instance.session_id)
            .is_none());
        receiver_task.abort();
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn closed_direct_receiver_falls_back_to_synchronous_cleanup() {
        const SERVICE: &str = "closed-receiver-cleanup";
        const ENV: &str = "test";

        let server = SidecarServer::default();
        let (sender, receiver) = crate::service::telemetry::direct_telemetry_channel(&server);
        drop(receiver);
        server.install_direct_telemetry_sender(sender);

        let instance = InstanceId::new("session", "runtime");
        let session = server.get_session(&instance.session_id);
        session.get_runtime(&instance.runtime_id);
        server.metrics_logs_clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance,
            &RuntimeMetadata::new("php", "8.3", "test"),
            Config::default,
            Vec::new(),
        );

        server.stop_runtime(&instance).await;

        assert!(server
            .metrics_logs_clients
            .get_existing_metrics_logs(&instance, SERVICE, ENV)
            .is_none());
        assert!(session.find_runtime(&instance.runtime_id).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn non_fork_reconfiguration_recreates_direct_worker_with_new_endpoint() {
        const SERVICE: &str = "reconfigured-direct-worker";
        const ENV: &str = "test";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let old_server = MockServer::start_async().await;
        let new_server = MockServer::start_async().await;
        let old_log = old_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("before-reconfiguration");
                then.status(202);
            })
            .await;
        let stale_new_log = old_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("after-reconfiguration");
                then.status(202);
            })
            .await;
        let new_log = new_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("after-reconfiguration");
                then.status(202);
            })
            .await;

        let server = SidecarServer::default();
        let (sender, receiver) = crate::service::telemetry::direct_telemetry_channel(&server);
        server.install_direct_telemetry_sender(sender.clone());
        let receiver_task = tokio::spawn(telemetry_action_receiver_task(server.clone(), receiver));
        let handler = test_handler(server.clone());
        handler
            .set_session_config(
                "session".to_string(),
                #[cfg(windows)]
                crate::service::RemoteConfigNotifyFunction::default(),
                test_session_config(
                    Endpoint {
                        url: old_server.url("/").parse().unwrap(),
                        ..Endpoint::default()
                    },
                    SERVICE,
                ),
                true,
            )
            .await;

        let instance = InstanceId::new("session", "runtime");
        server.get_runtime(&instance);
        let direct_log = |message: &str| InternalTelemetryActions {
            instance_id: instance.clone(),
            service_name: SERVICE.to_string(),
            env_name: ENV.to_string(),
            actions: vec![
                crate::service::telemetry::InternalTelemetryAction::TelemetryAction(
                    TelemetryActions::AddLog((
                        libdd_telemetry::worker::LogIdentifier { identifier: 7 },
                        libdd_telemetry::data::Log {
                            message: message.to_string(),
                            level: libdd_telemetry::data::LogLevel::Debug,
                            count: 1,
                            stack_trace: None,
                            tags: String::new(),
                            is_sensitive: false,
                            is_crash: false,
                        },
                    )),
                ),
                crate::service::telemetry::InternalTelemetryAction::TelemetryAction(
                    TelemetryActions::Lifecycle(LifecycleAction::FlushData),
                ),
            ],
        };
        sender
            .send_actions(direct_log("before-reconfiguration"))
            .await
            .unwrap();
        timeout(TokioDuration::from_secs(5), async {
            while old_log.calls_async().await != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old worker should deliver to the original endpoint");

        handler
            .set_session_config(
                "session".to_string(),
                #[cfg(windows)]
                crate::service::RemoteConfigNotifyFunction::default(),
                test_session_config(
                    Endpoint {
                        url: new_server.url("/").parse().unwrap(),
                        ..Endpoint::default()
                    },
                    SERVICE,
                ),
                false,
            )
            .await;
        server.get_runtime(&instance);
        sender
            .send_actions(direct_log("after-reconfiguration"))
            .await
            .unwrap();
        timeout(TokioDuration::from_secs(5), async {
            while new_log.calls_async().await != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement worker should deliver to the new endpoint");

        assert_eq!(old_log.calls_async().await, 1);
        assert_eq!(stale_new_log.calls_async().await, 0);
        assert_eq!(new_log.calls_async().await, 1);
        receiver_task.abort();
    }

    fn ffe_context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn ffe_exposure(subject_id: &str) -> FfeExposure {
        FfeExposure {
            timestamp_ms: 123,
            flag_key: "flag".to_owned(),
            subject_id: subject_id.to_owned(),
            subject_attributes_json: "{}".to_owned(),
            allocation_key: "alloc".to_owned(),
            variant: "variant".to_owned(),
        }
    }

    fn ffe_metric() -> FfeEvaluationMetric {
        FfeEvaluationMetric {
            flag_key: "flag".to_owned(),
            variant: "variant".to_owned(),
            reason: "TARGETING_MATCH".to_owned(),
            error_type: None,
            allocation_key: Some("alloc".to_owned()),
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ffe_exposure_actions_dispatch_without_registered_application() {
        let http_server = MockServer::start_async().await;
        let exposures_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(ffe_exposures_flusher::EVP_EXPOSURES_PATH);
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(42);

        handler
            .server
            .get_session(&instance_id.session_id)
            .modify_trace_config(|cfg| {
                let endpoint = Endpoint {
                    url: http_server.url("/").parse().unwrap(),
                    ..Endpoint::default()
                };
                cfg.set_endpoint(endpoint).unwrap();
            });

        assert!(!handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .contains_key(&queue_id));

        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![SidecarAction::FfeExposureBatch(FfeExposureBatch {
                    context: ffe_context(),
                    exposures: vec![ffe_exposure("user")],
                })],
            )
            .await;

        for _ in 0..100 {
            if exposures_mock.calls_async().await == 1 {
                break;
            }
            sleep(TokioDuration::from_millis(10)).await;
        }

        exposures_mock.assert_async().await;
        assert!(!handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .contains_key(&queue_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ffe_metric_actions_dispatch_without_registered_application() {
        let http_server = MockServer::start_async().await;
        let test_session_token = "ffe/evaluation_metrics_sidecar";
        let metrics_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/metrics")
                    .header("x-datadog-test-session-token", test_session_token);
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(42);

        handler
            .server
            .get_session(&instance_id.session_id)
            .modify_otlp_metrics_endpoint(|endpoint| {
                *endpoint = Some(Endpoint {
                    url: http_server.url("/v1/metrics").parse().unwrap(),
                    test_token: Some(test_session_token.into()),
                    ..Endpoint::default()
                });
            });

        assert!(!handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .contains_key(&queue_id));

        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![SidecarAction::FfeEvaluationMetrics {
                    context: ffe_context(),
                    metrics: vec![ffe_metric()],
                }],
            )
            .await;

        for _ in 0..100 {
            if metrics_mock.calls_async().await == 1 {
                break;
            }
            sleep(TokioDuration::from_millis(10)).await;
        }

        metrics_mock.assert_async().await;
        assert!(!handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .contains_key(&queue_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn registered_sdk_without_ffe_actions_does_not_emit_ffe_telemetry() {
        let http_server = MockServer::start_async().await;
        let exposures_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(ffe_exposures_flusher::EVP_EXPOSURES_PATH);
                then.status(202);
            })
            .await;
        let metrics_mock = http_server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/metrics");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(42);

        handler
            .server
            .get_session(&instance_id.session_id)
            .modify_trace_config(|cfg| {
                let endpoint = Endpoint {
                    url: http_server.url("/").parse().unwrap(),
                    ..Endpoint::default()
                };
                cfg.set_endpoint(endpoint).unwrap();
            });

        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default();

        assert!(handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .contains_key(&queue_id));

        handler
            .enqueue_actions(instance_id, queue_id, Vec::new())
            .await;

        sleep(TokioDuration::from_millis(50)).await;

        assert_eq!(exposures_mock.calls_async().await, 0);
        assert_eq!(metrics_mock.calls_async().await, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn composer_before_config_waits_for_configured_app_started() {
        const SERVICE: &str = "composer-before-config";
        const ENV: &str = "test";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let configured_start = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_includes("\"name\":\"php_config\"");
                then.status(202);
            })
            .await;
        let empty_start = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_excludes("\"name\":\"php_config\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(1);
        let session = handler.server.get_session(&instance_id.session_id);
        *session.session_config.lock_or_panic() = Some({
            let mut config = Config::default();
            config
                .set_endpoint_uri(http_server.url("/").parse().unwrap())
                .unwrap();
            config
        });
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                ENV.to_string(),
                String::new(),
                SERVICE.to_string(),
                Vec::new(),
            );

        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![SidecarAction::PhpComposerTelemetryFile(PathBuf::from(
                    "/missing/vendor/composer/installed.json",
                ))],
            )
            .await;
        sleep(TokioDuration::from_millis(50)).await;
        assert_eq!(configured_start.calls_async().await, 0);
        assert_eq!(empty_start.calls_async().await, 0);

        handler
            .enqueue_actions(
                instance_id,
                queue_id,
                vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                    libdd_telemetry::data::Configuration {
                        name: "php_config".to_string(),
                        value: "present".to_string(),
                        origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                        config_id: None,
                        seq_id: None,
                    },
                ))],
            )
            .await;

        timeout(TokioDuration::from_secs(5), async {
            while configured_start.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("configured app-started request");
        assert_eq!(empty_start.calls_async().await, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn composer_before_terminal_stop_is_delivered_before_app_closing() {
        const SERVICE: &str = "composer-before-terminal-stop";
        const ENV: &str = "test";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let app_started = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"");
                then.status(202);
            })
            .await;
        let terminal_batch = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-dependencies-loaded\"")
                    .body_includes("\"name\":\"datadog/dd-trace\"")
                    .body_includes("\"request_type\":\"app-closing\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(1);
        let session = handler.server.get_session(&instance_id.session_id);
        *session.session_config.lock_or_panic() = Some({
            let mut config = Config::default();
            config
                .set_endpoint_uri(http_server.url("/").parse().unwrap())
                .unwrap();
            config
        });
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                ENV.to_string(),
                String::new(),
                SERVICE.to_string(),
                Vec::new(),
            );

        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![SidecarAction::PhpComposerTelemetryFile(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/installed.json"),
                )],
            )
            .await;
        handler
            .enqueue_actions(
                instance_id,
                queue_id,
                vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                    LifecycleAction::Stop,
                ))],
            )
            .await;

        let delivered = timeout(TokioDuration::from_secs(5), async {
            loop {
                if app_started.calls_async().await == 1 && terminal_batch.calls_async().await == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        if delivered.is_err() {
            panic!(
                "expected Start=1 and Composer+Stop batch=1; observed Start={}, batch={}",
                app_started.calls_async().await,
                terminal_batch.calls_async().await,
            )
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[cfg_attr(miri, ignore)]
    async fn stop_race_returns_untouched_actions_to_the_next_lifecycle() {
        const SERVICE: &str = "stop-enqueue-race";
        const ENV: &str = "test";

        let clients = TelemetryCachedClientSet::default();
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let instance = InstanceId::new("session", "runtime");
        let active = clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &instance,
                    &runtime_metadata,
                    Vec::new(),
                ),
                Config::default,
                InitialTelemetryData::default(),
            )
            .expect("active application telemetry worker");
        let selected = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));

        let racing_clients = clients.clone();
        let racing_runtime = runtime_metadata.clone();
        let racing_instance = instance.clone();
        let selected_by_enqueue = selected.clone();
        let resume_enqueue = resume.clone();
        let enqueue = tokio::spawn(async move {
            let dispatch = racing_clients.get_or_create_for_actions(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &racing_instance,
                    &racing_runtime,
                    Vec::new(),
                ),
                PendingApplicationAction::from_actions(
                    &racing_instance,
                    vec![SidecarAction::Telemetry(TelemetryActions::AddDependency(
                        libdd_telemetry::data::Dependency {
                            name: "survives-stop-race".to_string(),
                            version: None,
                        },
                    ))],
                    &HashMap::new(),
                ),
                Config::default,
                |_, _| panic!("the active lifecycle should already exist"),
            );
            let ApplicationTelemetryDispatch::Ready {
                client,
                actions,
                created,
                ..
            } = dispatch
            else {
                panic!("the racing batch should select the active lifecycle");
            };
            selected_by_enqueue.wait().await;
            resume_enqueue.wait().await;
            schedule_application_actions(
                &client,
                actions,
                created,
                &racing_instance,
                QueueId::from(1),
            )
            .expect_err("a stopped lifecycle must return the untouched owned actions")
        });

        selected.wait().await;
        let stop_result = schedule_application_actions(
            &active,
            PendingApplicationAction::from_actions(
                &instance,
                vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                    LifecycleAction::Stop,
                ))],
                &HashMap::new(),
            ),
            false,
            &instance,
            QueueId::from(1),
        )
        .expect("Stop should schedule on the selected active lifecycle");
        assert!(stop_result.remove_client);
        clients.remove_telemetry_client(SERVICE, ENV, &active);
        resume.wait().await;

        let returned_actions = enqueue.await.expect("racing enqueue task");
        assert_eq!(returned_actions.len(), 1);
        let promoted = Arc::new(Mutex::new(Vec::new()));
        let promoted_by_initializer = promoted.clone();
        let mut next_actions = returned_actions;
        next_actions.extend(PendingApplicationAction::from_actions(
            &instance,
            vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                libdd_telemetry::data::Configuration {
                    name: "next-lifecycle".to_string(),
                    value: "configured".to_string(),
                    origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                    config_id: None,
                    seq_id: None,
                },
            ))],
            &HashMap::new(),
        ));
        let dispatch = clients.get_or_create_for_actions(
            TelemetryWorkerMetadata::new(SERVICE, ENV, &instance, &runtime_metadata, Vec::new()),
            next_actions,
            Config::default,
            move |_, actions| {
                promoted_by_initializer
                    .lock_or_panic()
                    .extend(actions.into_iter().map(|pending| pending.action));
                false
            },
        );
        assert!(matches!(
            dispatch,
            ApplicationTelemetryDispatch::Ready { created: true, .. }
        ));
        assert!(promoted.lock_or_panic().iter().any(|action| matches!(
            action,
            SidecarAction::Telemetry(TelemetryActions::AddDependency(dependency))
                if dependency.name == "survives-stop-race"
        )));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn terminal_initialization_returns_suffix_to_the_next_lifecycle() {
        const SERVICE: &str = "initial-stop-suffix";
        const ENV: &str = "test";

        let clients = TelemetryCachedClientSet::default();
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let instance = InstanceId::new("session", "runtime");
        let next_configuration = libdd_telemetry::data::Configuration {
            name: "next-lifecycle".to_string(),
            value: "configured".to_string(),
            origin: libdd_telemetry::data::ConfigurationOrigin::Default,
            config_id: None,
            seq_id: None,
        };
        let actions = PendingApplicationAction::from_actions(
            &instance,
            vec![
                SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop)),
                SidecarAction::Telemetry(TelemetryActions::AddConfig(next_configuration)),
            ],
            &HashMap::new(),
        );

        let dispatch = clients.get_or_create_for_actions(
            TelemetryWorkerMetadata::new(SERVICE, ENV, &instance, &runtime_metadata, Vec::new()),
            actions,
            Config::default,
            |client, actions| {
                schedule_application_actions(client, actions, true, &instance, QueueId::from(1))
                    .expect("the initial lifecycle should accept its terminal prefix")
                    .remove_client
            },
        );

        let ApplicationTelemetryDispatch::Ready {
            actions,
            remove_client,
            ..
        } = dispatch
        else {
            panic!("Stop should promote the initial lifecycle");
        };
        assert!(remove_client);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].action,
            SidecarAction::Telemetry(TelemetryActions::AddConfig(configuration))
                if configuration.name == "next-lifecycle"
        ));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_state_application_action_retries_due_shared_memory_creation() {
        const SERVICE: &str = "retry-on-dependency";
        const ENV: &str = "test";

        let retry_started = Instant::now() - Duration::from_secs(60);
        let client = TelemetryCachedClient::new_with_shm_factory(
            TelemetryWorkerMetadata::new(
                SERVICE,
                ENV,
                &InstanceId::new("session", "runtime"),
                &RuntimeMetadata::new("php", "8.3", "test"),
                Vec::new(),
            ),
            Config::default,
            InitialTelemetryData::default(),
            retry_started,
            |_| Err(std::io::Error::other("injected initial failure")),
        )
        .expect("worker should start even when initial SHM creation fails");
        let client = Arc::new(Mutex::new(Some(client)));

        let _ = schedule_application_actions(
            &client,
            PendingApplicationAction::from_actions(
                &InstanceId::new("session", "runtime"),
                vec![SidecarAction::Telemetry(TelemetryActions::AddDependency(
                    libdd_telemetry::data::Dependency {
                        name: "retry-trigger".to_string(),
                        version: None,
                    },
                ))],
                &HashMap::new(),
            ),
            false,
            &InstanceId::new("session", "runtime"),
            QueueId::from(1),
        );

        assert!(matches!(
            client.lock_or_panic().as_ref(),
            Some(application_client) if application_client.has_ready_shm()
        ));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn pending_startup_data_is_seeded_before_start() {
        const SERVICE: &str = "startup-data-before-config";
        const ENV: &str = "test";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let any_start = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"");
                then.status(202);
            })
            .await;
        let complete_start = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_includes("\"name\":\"startup-dependency\"")
                    .body_includes("\"name\":\"startup-integration\"")
                    .body_includes("\"name\":\"startup-config\"");
                then.status(202);
            })
            .await;
        let dependency_change = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-dependencies-loaded\"");
                then.status(202);
            })
            .await;
        let integration_change = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-integrations-change\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(1);
        let session = handler.server.get_session(&instance_id.session_id);
        *session.session_config.lock_or_panic() = Some({
            let mut config = Config::default();
            config
                .set_endpoint_uri(http_server.url("/").parse().unwrap())
                .unwrap();
            config
        });
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                ENV.to_string(),
                String::new(),
                SERVICE.to_string(),
                Vec::new(),
            );

        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![
                    SidecarAction::Telemetry(TelemetryActions::AddDependency(
                        libdd_telemetry::data::Dependency {
                            name: "startup-dependency".to_string(),
                            version: None,
                        },
                    )),
                    SidecarAction::Telemetry(TelemetryActions::AddIntegration(
                        libdd_telemetry::data::Integration {
                            name: "startup-integration".to_string(),
                            enabled: true,
                            version: None,
                            compatible: None,
                            auto_enabled: None,
                        },
                    )),
                ],
            )
            .await;
        sleep(TokioDuration::from_millis(50)).await;
        assert_eq!(any_start.calls_async().await, 0);
        assert_eq!(complete_start.calls_async().await, 0);
        assert_eq!(dependency_change.calls_async().await, 0);
        assert_eq!(integration_change.calls_async().await, 0);
        any_start.delete_async().await;

        handler
            .enqueue_actions(
                instance_id,
                queue_id,
                vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                    libdd_telemetry::data::Configuration {
                        name: "startup-config".to_string(),
                        value: "present".to_string(),
                        origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                        config_id: None,
                        seq_id: None,
                    },
                ))],
            )
            .await;

        timeout(TokioDuration::from_secs(5), async {
            while complete_start.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("complete app-started request");

        let worker = handler
            .server
            .telemetry_clients
            .workers()
            .into_iter()
            .next()
            .expect("application telemetry worker");
        worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
            .await
            .unwrap();
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .unwrap();
        rx.await.unwrap();

        assert_eq!(complete_start.calls_async().await, 1);
        assert_eq!(dependency_change.calls_async().await, 0);
        assert_eq!(integration_change.calls_async().await, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn pending_metric_point_uses_originating_runtime_registration() {
        const SERVICE: &str = "pending-metric-origin";
        const ENV: &str = "test";
        const METRIC: &str = "originating.runtime.metric";

        let server = SidecarServer::default();
        let origin_handler = test_handler(server.clone());
        let promoting_handler = test_handler(server.clone());
        let origin_instance = InstanceId::new("session", "origin-runtime");
        let promoting_instance = InstanceId::new("session", "promoting-runtime");
        let origin_queue = QueueId::from(1);
        let promoting_queue = QueueId::from(2);

        *server.get_session("session").session_config.lock_or_panic() = Some(Config::default());

        for (instance_id, queue_id) in [
            (&origin_instance, origin_queue),
            (&promoting_instance, promoting_queue),
        ] {
            server
                .get_runtime(instance_id)
                .lock_applications()
                .entry(queue_id)
                .or_default()
                .set_metadata(
                    ENV.to_string(),
                    String::new(),
                    SERVICE.to_string(),
                    Vec::new(),
                );
        }

        origin_handler
            .register_telemetry_metric(MetricContext {
                name: METRIC.to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            })
            .await;
        origin_handler
            .enqueue_actions(
                origin_instance,
                origin_queue,
                vec![SidecarAction::AddTelemetryMetricPoint((
                    METRIC.to_string(),
                    1.0,
                    Vec::new(),
                ))],
            )
            .await;

        promoting_handler
            .enqueue_actions(
                promoting_instance,
                promoting_queue,
                vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                    libdd_telemetry::data::Configuration {
                        name: "startup-config".to_string(),
                        value: "present".to_string(),
                        origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                        config_id: None,
                        seq_id: None,
                    },
                ))],
            )
            .await;

        let app_client = server
            .telemetry_clients
            .clients()
            .into_iter()
            .next()
            .expect("promoting config should create the application worker");
        assert!(app_client
            .lock_or_panic()
            .as_ref()
            .expect("application telemetry client")
            .telemetry_metrics
            .contains_key(METRIC));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn initial_config_reaches_app_started_through_enqueue_actions() {
        const CLIENTS: usize = 16;
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let app_started_with_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_includes("\"name\":\"race_config\"")
                    .body_includes("\"name\":\"race_config_second\"");
                then.status(202);
            })
            .await;
        let app_started_without_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_excludes("\"name\":\"race_config\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let session = handler.server.get_session("session");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);

        for index in 0..CLIENTS {
            let service = format!("telemetry-enqueue-race-{index}");
            let instance_id = InstanceId::new("session", &format!("runtime-{index}"));
            let queue_id = QueueId::from(index as u64 + 1);
            handler
                .server
                .get_runtime(&instance_id)
                .lock_applications()
                .entry(queue_id)
                .or_default()
                .set_metadata(String::new(), String::new(), service, Vec::new());

            handler
                .enqueue_actions(
                    instance_id,
                    queue_id,
                    vec![
                        SidecarAction::Telemetry(TelemetryActions::AddDependency(
                            libdd_telemetry::data::Dependency {
                                name: "startup-dependency".to_string(),
                                version: None,
                            },
                        )),
                        SidecarAction::Telemetry(TelemetryActions::AddConfig(
                            libdd_telemetry::data::Configuration {
                                name: "race_config".to_string(),
                                value: "present".to_string(),
                                origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                                config_id: None,
                                seq_id: None,
                            },
                        )),
                        SidecarAction::Telemetry(TelemetryActions::AddConfig(
                            libdd_telemetry::data::Configuration {
                                name: "race_config_second".to_string(),
                                value: "present".to_string(),
                                origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                                config_id: None,
                                seq_id: None,
                            },
                        )),
                    ],
                )
                .await;
        }

        tokio::time::timeout(TokioDuration::from_secs(10), async {
            loop {
                let with_config = app_started_with_config.calls_async().await;
                let without_config = app_started_without_config.calls_async().await;
                if with_config + without_config == CLIENTS {
                    break;
                }
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("all app-started requests should arrive");

        let missing = app_started_without_config.calls_async().await;
        assert_eq!(
            missing, 0,
            "{missing} app-started payloads raced ahead of their initial config"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn stats_concentrator_does_not_start_app_telemetry_before_config() {
        const SERVICE: &str = "stats-before-config";
        const ENV: &str = "test-env";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let app_started_with_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_includes("\"name\":\"stats_race_config\"");
                then.status(202);
            })
            .await;
        let app_started_without_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_excludes("\"name\":\"stats_race_config\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let session = handler.server.get_session("session");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint::default(),
            flush_interval: Duration::from_secs(60),
            hostname: String::new(),
            process_tags: String::new(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });

        let instance_id = InstanceId::new("session", "runtime");
        let queue_id = QueueId::from(1);
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                ENV.to_string(),
                String::new(),
                SERVICE.to_string(),
                Vec::new(),
            );

        let concentrator =
            get_or_create_concentrator(&handler.server.span_concentrators, ENV, "", &session)
                .expect("stats concentrator");

        let worker = concentrator
            .telemetry
            .as_ref()
            .expect("stats telemetry worker");
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .unwrap();
        rx.await.unwrap();
        assert_eq!(
            app_started_without_config.calls_async().await,
            0,
            "stats creation started app telemetry before tracer config arrived"
        );

        handler
            .enqueue_actions(
                instance_id,
                queue_id,
                vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                    libdd_telemetry::data::Configuration {
                        name: "stats_race_config".to_string(),
                        value: "present".to_string(),
                        origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                        config_id: None,
                        seq_id: None,
                    },
                ))],
            )
            .await;

        tokio::time::timeout(TokioDuration::from_secs(10), async {
            while app_started_with_config.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("configured app-started request should arrive");

        assert_eq!(app_started_without_config.calls_async().await, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn telemetry_flush_includes_stats_workers() {
        const SERVICE: &str = "stats-worker-flush";
        const ENV: &str = "test-env";
        const METRIC: &str = "stats_worker.flush_test";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let metric_request = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes(format!("\"metric\":\"{METRIC}\""));
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let session = handler.server.get_session("session");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint::default(),
            flush_interval: Duration::from_secs(60),
            hostname: String::new(),
            process_tags: String::new(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });

        let state =
            get_or_create_concentrator(&handler.server.span_concentrators, ENV, "", &session)
                .expect("stats concentrator");
        let worker = state.telemetry.as_ref().expect("stats telemetry worker");
        let context = worker.register_metric_context(
            METRIC.to_string(),
            Vec::new(),
            libdd_telemetry::data::metrics::MetricType::Count,
            true,
            libdd_telemetry::data::metrics::MetricNamespace::Tracers,
        );
        worker.add_point(1.0, &context, Vec::new()).unwrap();

        handler
            .flush(SidecarFlushOptions {
                traces_and_stats: true,
                telemetry: true,
            })
            .await;

        assert_eq!(
            metric_request.calls_async().await,
            1,
            "flush returned before the dedicated stats telemetry worker sent its metric"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn explicit_stats_flush_reuses_exporter_metric_context() {
        const SERVICE: &str = "stats-exporter-reuse";
        const ENV: &str = "stats-exporter-reuse-env";

        let server = SidecarServer::default();
        let session = server.get_session("stats-exporter-reuse-session");
        *session.session_config.lock_or_panic() = Some(Config::default());
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint::default(),
            flush_interval: Duration::from_secs(3600),
            hostname: String::new(),
            process_tags: String::new(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });
        let state = get_or_create_concentrator(&server.span_concentrators, ENV, "", &session)
            .expect("stats concentrator");
        tokio::task::yield_now().await;

        let worker = state.telemetry.as_ref().expect("stats telemetry worker");
        let before = worker
            .stats()
            .expect("stats collection request")
            .await
            .expect("stats collection response")
            .metric_contexts;

        flush_all_stats_now(std::slice::from_ref(&state)).await;
        flush_all_stats_now(std::slice::from_ref(&state)).await;

        let after = worker
            .stats()
            .expect("stats collection request")
            .await
            .expect("stats collection response")
            .metric_contexts;
        assert_eq!(
            after, before,
            "manual flushes must reuse the state-owned exporter telemetry context"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn stats_worker_uses_auxiliary_identity_without_registering_runtime() {
        const SERVICE: &str = "stats-auxiliary";
        const ENV: &str = "stats-auxiliary-env";
        const METRIC: &str = "stats_auxiliary.metric";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let server = SidecarServer::default();
        let session = server.get_session("stats-auxiliary-session");
        session.get_runtime(&"application-runtime".to_string());
        *session.process_tags.lock_or_panic() =
            vec![Tag::new("custom", "value").expect("valid test tag")];
        *session.auto_resolved_service_name.lock_or_panic() = Some(SERVICE.to_string());
        let expected_process_tags = session
            .process_tags_with_svc_source()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint::default(),
            flush_interval: Duration::from_secs(3600),
            hostname: String::new(),
            process_tags: expected_process_tags.clone(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });
        let runtime_count_before = session.lock_runtimes().len();
        let sidecar_runtime_count_before = server.compute_stats().await.runtimes;

        let state = get_or_create_concentrator(&server.span_concentrators, ENV, "", &session)
            .expect("stats concentrator");

        assert_eq!(session.lock_runtimes().len(), runtime_count_before);
        assert_eq!(
            server.compute_stats().await.runtimes,
            sidecar_runtime_count_before,
            "the auxiliary stats worker must not appear in SidecarStats.runtimes"
        );
        assert!(state.meta.runtime_id.starts_with("stats-"));
        assert_ne!(state.meta.runtime_id, "application-runtime");
        assert_ne!(state.meta.runtime_id, "caller-runtime");
        assert_eq!(state.meta.process_tags, expected_process_tags);

        let metric_request = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes(format!("\"metric\":\"{METRIC}\""))
                    .body_includes(format!("\"runtime_id\":\"{}\"", state.meta.runtime_id))
                    .body_includes(format!("\"process_tags\":\"{expected_process_tags}\""));
                then.status(202);
            })
            .await;
        let worker = state.telemetry.as_ref().expect("stats telemetry worker");
        let context = worker.register_metric_context(
            METRIC.to_string(),
            Vec::new(),
            libdd_telemetry::data::metrics::MetricType::Count,
            true,
            libdd_telemetry::data::metrics::MetricNamespace::Tracers,
        );
        worker.add_point(1.0, &context, Vec::new()).unwrap();
        worker
            .send_msg(TelemetryActions::Lifecycle(
                LifecycleAction::FlushMetricAggr,
            ))
            .await
            .unwrap();
        worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
            .await
            .unwrap();
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .unwrap();
        rx.await.unwrap();

        assert_eq!(
            metric_request.calls_async().await,
            1,
            "the auxiliary worker should use the synthetic runtime id and full process tags"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn stats_flush_snapshots_concentrators_after_trace_flush() {
        const SERVICE: &str = "concurrent-stats-worker";
        const ENV: &str = "test-env";

        let trace_server = MockServer::start_async().await;
        let trace_request = trace_server
            .mock_async(|when, then| {
                when.method(POST);
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#)
                    .delay(TokioDuration::from_millis(200));
            })
            .await;
        let stats_server = MockServer::start_async().await;
        let stats_request = stats_server
            .mock_async(|when, then| {
                when.method(POST).path("/v0.6/stats");
                then.status(202);
            })
            .await;

        let server = SidecarServer::default();
        let trace_endpoint = Endpoint {
            url: trace_server.url("/").parse().unwrap(),
            ..Endpoint::default()
        };
        server
            .trace_flusher
            .enqueue(create_send_data(128, &trace_endpoint));
        let handler = test_handler(server.clone());
        let flush = tokio::spawn(async move {
            handler
                .flush(SidecarFlushOptions {
                    traces_and_stats: true,
                    telemetry: false,
                })
                .await;
        });

        tokio::time::timeout(TokioDuration::from_secs(5), async {
            while trace_request.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("trace flush should be in flight");

        let session = server.get_session("session");
        *session.session_config.lock_or_panic() = Some(Config::default());
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint {
                url: stats_server.url("/v0.6/stats").parse().unwrap(),
                ..Endpoint::default()
            },
            flush_interval: Duration::from_secs(60),
            hostname: String::new(),
            process_tags: String::new(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });
        let state = get_or_create_concentrator(&server.span_concentrators, ENV, "", &session)
            .expect("stats concentrator");
        state.concentrator.add_span(&ShmSpanInput {
            fixed: FixedAggregationKey {
                service_name: SERVICE,
                resource_name: "resource",
                operation_name: "operation",
                span_type: "web",
                span_kind: "server",
                http_method: "GET",
                http_endpoint: "/",
                service_source: "",
                http_status_code: 200,
                is_synthetics_request: false,
                is_trace_root: Default::default(),
                grpc_status_code: None,
            },
            peer_tags: &[],
            duration_ns: 1_000_000,
            is_error: false,
            is_top_level: true,
        });

        flush.await.unwrap();
        assert_eq!(
            stats_request.calls_async().await,
            1,
            "a concentrator created during trace flush should be included"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn compute_stats_preserves_application_client_count() {
        const SERVICE: &str = "worker-stats";
        const ENV: &str = "test-env";

        let http_server = MockServer::start_async().await;
        let _telemetry = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/telemetry/proxy/api/v2/apmtelemetry");
                then.status(202);
            })
            .await;
        let server = SidecarServer::default();
        let instance_id = InstanceId::new("session", "runtime");
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();

        let app_client = server
            .telemetry_clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &instance_id,
                    &runtime_metadata,
                    Vec::new(),
                ),
                || telemetry_config.clone(),
                InitialTelemetryData::default(),
            )
            .expect("application telemetry worker");
        app_client
            .lock_or_panic()
            .as_mut()
            .expect("application telemetry client")
            .register_metric(MetricContext {
                name: "app.metric".to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            });

        let metrics_logs_client = server.metrics_logs_clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance_id,
            &runtime_metadata,
            || telemetry_config.clone(),
            Vec::new(),
        );
        metrics_logs_client
            .lock_or_panic()
            .as_mut()
            .expect("metrics/logs telemetry client")
            .register_metric(MetricContext {
                name: "internal.metric".to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            });

        let session = server.get_session(&instance_id.session_id);
        *session.session_config.lock_or_panic() = Some(telemetry_config);
        *session.stats_config.lock_or_panic() = Some(StatsConfig {
            endpoint: Endpoint::default(),
            flush_interval: Duration::from_secs(60),
            hostname: String::new(),
            process_tags: String::new(),
            root_service: SERVICE.to_string(),
            language: "php".to_string(),
            tracer_version: "test".to_string(),
        });
        let stats_state = get_or_create_concentrator(&server.span_concentrators, ENV, "", &session)
            .expect("stats concentrator");
        stats_state
            .telemetry
            .as_ref()
            .expect("stats telemetry worker")
            .register_metric_context(
                "stats.metric".to_string(),
                Vec::new(),
                libdd_telemetry::data::metrics::MetricType::Count,
                true,
                libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            );

        let stats = server.compute_stats().await;
        assert_eq!(stats.active_telemetry_clients, 1);
        assert_eq!(stats.telemetry_metrics_contexts, 2);
        assert_eq!(
            stats.telemetry_worker.metric_contexts, 4,
            "application, direct, stats-test, and state-owned exporter contexts are counted"
        );
        assert_eq!(stats.telemetry_worker_errors, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn initial_stop_follows_app_started() {
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let app_started = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"");
                then.status(202).delay(TokioDuration::from_millis(200));
            })
            .await;
        let app_closing = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-closing\"");
                then.status(202);
            })
            .await;

        let handler = test_handler(SidecarServer::default());
        let session = handler.server.get_session("session");
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);

        let instance_id = InstanceId::new("session", "stop-runtime");
        let queue_id = QueueId::from(1);
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                String::new(),
                String::new(),
                "stop-service".to_string(),
                Vec::new(),
            );

        handler
            .enqueue_actions(
                instance_id,
                queue_id,
                vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                    LifecycleAction::Stop,
                ))],
            )
            .await;

        tokio::time::timeout(TokioDuration::from_secs(10), async {
            while app_started.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("app-started request should arrive");

        assert_eq!(
            app_closing.calls_async().await,
            0,
            "app-closing arrived before the delayed app-started response completed"
        );

        tokio::time::timeout(TokioDuration::from_secs(10), async {
            while app_closing.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("app-closing request should arrive after app-started");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn successor_lifecycle_waits_for_predecessor_app_closing() {
        const SERVICE: &str = "ordered-successor";
        const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

        let http_server = MockServer::start_async().await;
        let app_started = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"");
                then.status(202);
            })
            .await;
        let app_closing = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-closing\"");
                then.status(202).delay(TokioDuration::from_millis(300));
            })
            .await;

        let handler = Arc::new(test_handler(SidecarServer::default()));
        let instance_id = InstanceId::new("session", "successor-runtime");
        let queue_id = QueueId::from(1);
        let session = handler.server.get_session(&instance_id.session_id);
        let mut telemetry_config = Config::default();
        telemetry_config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        *session.session_config.lock_or_panic() = Some(telemetry_config);
        handler
            .server
            .get_runtime(&instance_id)
            .lock_applications()
            .entry(queue_id)
            .or_default()
            .set_metadata(
                String::new(),
                String::new(),
                SERVICE.to_string(),
                Vec::new(),
            );

        let configuration = |name: &str| {
            SidecarAction::Telemetry(TelemetryActions::AddConfig(
                libdd_telemetry::data::Configuration {
                    name: name.to_string(),
                    value: "present".to_string(),
                    origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                    config_id: None,
                    seq_id: None,
                },
            ))
        };
        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![configuration("first-lifecycle")],
            )
            .await;
        timeout(TokioDuration::from_secs(5), async {
            while app_started.calls_async().await != 1 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("first app-started request");

        let handoff_started = Instant::now();
        handler
            .enqueue_actions(
                instance_id.clone(),
                queue_id,
                vec![
                    SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop)),
                    configuration("second-lifecycle"),
                ],
            )
            .await;
        assert!(
            handoff_started.elapsed() >= TokioDuration::from_millis(300),
            "successor dispatch returned before the predecessor app-closing request completed"
        );
        assert_eq!(app_closing.calls_async().await, 1);
        timeout(TokioDuration::from_secs(5), async {
            while app_started.calls_async().await != 2 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("successor app-started request");

        let stop_handler = handler.clone();
        let stop_instance = instance_id.clone();
        let stop = tokio::spawn(async move {
            stop_handler
                .enqueue_actions(
                    stop_instance,
                    queue_id,
                    vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                        LifecycleAction::Stop,
                    ))],
                )
                .await;
        });
        timeout(TokioDuration::from_secs(5), async {
            while app_closing.calls_async().await != 2 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("second predecessor app-closing request");

        let successor_handler = handler.clone();
        let successor = tokio::spawn(async move {
            successor_handler
                .enqueue_actions(
                    instance_id,
                    queue_id,
                    vec![configuration("concurrent-successor")],
                )
                .await;
        });
        sleep(TokioDuration::from_millis(50)).await;
        assert_eq!(
            app_started.calls_async().await,
            2,
            "a concurrent successor bypassed the in-progress terminal handoff"
        );

        stop.await.expect("concurrent stop task");
        successor.await.expect("concurrent successor task");
        timeout(TokioDuration::from_secs(5), async {
            while app_started.calls_async().await != 3 {
                sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("concurrent successor app-started request");
    }
}

// TODO: APMSP-1079 - Unit tests are sparse for the sidecar server. We should add more.
