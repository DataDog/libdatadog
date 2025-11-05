// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::{TemporarilyRetainedMapStats, MULTI_LOG_FILTER, MULTI_LOG_WRITER};
use crate::service::{
    sidecar_interface::ServeSidecarInterface,
    telemetry::{TelemetryCachedClient, TelemetryCachedClientSet},
    tracing::TraceFlusher,
    InstanceId, QueueId, RequestIdentification, RequestIdentifier, RuntimeInfo, RuntimeMetadata,
    SerializedTracerHeaderTags, SessionConfig, SessionInfo, SidecarAction, SidecarInterface,
    SidecarInterfaceRequest, SidecarInterfaceResponse,
};
use datadog_ipc::platform::{AsyncChannel, ShmHandle};
use datadog_ipc::tarpc;
use datadog_ipc::tarpc::context::Context;
use datadog_ipc::transport::Transport;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::tracer_payload::decode_to_trace_chunks;
use datadog_trace_utils::tracer_payload::TraceEncoding;
use ddcommon::{Endpoint, MutexExt};
use ddtelemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerStats};
use futures::future;
use futures::future::Ready;
use manual_future::ManualFutureCompleter;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};

use futures::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::task::{JoinError, JoinHandle};

use crate::config::get_product_endpoint;
use crate::service::agent_info::AgentInfos;
use crate::service::debugger_diagnostics_bookkeeper::{
    DebuggerDiagnosticsBookkeeper, DebuggerDiagnosticsBookkeeperStats,
};
use crate::service::exception_hash_rate_limiter::EXCEPTION_HASH_LIMITER;
use crate::service::remote_configs::{RemoteConfigNotifyTarget, RemoteConfigs};
use crate::service::tracing::trace_flusher::TraceFlusherStats;
use datadog_ipc::platform::FileBackedHandle;
use datadog_ipc::tarpc::server::{Channel, InFlightRequest};
use datadog_live_debugger::sender::DebuggerType;
use datadog_remote_config::fetch::{ConfigInvariants, MultiTargetStats};
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use ddcommon::tag::Tag;
use ddtelemetry::config::Config;
use libdd_dogstatsd_client::{new, DogStatsDActionOwned};
use libdd_tinybytes as tinybytes;

type NoResponse = Ready<()>;

fn no_response() -> NoResponse {
    future::ready(())
}

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

#[cfg(windows)]
#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct ProcessHandle(pub winapi::um::winnt::HANDLE);

#[cfg(windows)]
unsafe impl Send for ProcessHandle {}
#[cfg(windows)]
unsafe impl Sync for ProcessHandle {}

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
        Arc<Mutex<Option<ManualFutureCompleter<ddtelemetry::config::Config>>>>,
    /// Keeps track of the number of submitted payloads.
    pub(crate) submitted_payloads: Arc<AtomicU64>,
    /// All tracked agent infos per endpoint
    pub agent_infos: AgentInfos,
    /// All remote config handling
    remote_configs: RemoteConfigs,
    /// Diagnostics bookkeeper
    debugger_diagnostics_bookkeeper: Arc<DebuggerDiagnosticsBookkeeper>,
    /// The ProcessHandle tied to the connection
    #[cfg(windows)]
    process_handle: Option<ProcessHandle>,
}

impl SidecarServer {
    /// Accepts a new connection and starts processing requests.
    ///
    /// This function creates a new `tarpc` server with the provided `async_channel` and starts
    /// processing incoming requests. It also starts a session interceptor to keep track of active
    /// sessions and submitted payload counts.
    ///
    /// # Arguments
    ///
    /// * `async_channel`: An `AsyncChannel` that represents the connection to the client.
    #[cfg_attr(not(windows), allow(unused_mut))]
    pub async fn accept_connection(mut self, async_channel: AsyncChannel) {
        let handle = async_channel.handle();
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        {
            self.process_handle = async_channel
                .metadata
                .lock()
                .unwrap()
                .process_handle()
                .map(|p| ProcessHandle(p as winapi::um::winnt::HANDLE));
        }
        let server = tarpc::server::BaseChannel::new(
            tarpc::server::Config {
                pending_response_buffer: 10000,
            },
            Transport::from(async_channel),
        );
        let mut executor = datadog_ipc::sequential::execute_sequential(
            server.requests(),
            self.clone().serve(),
            500,
        );
        let (tx, rx) = tokio::sync::mpsc::channel::<_>(100);
        let tx = executor.swap_sender(tx);

        let session_interceptor = tokio::spawn(session_interceptor(
            self.session_counter.clone(),
            self.submitted_payloads.clone(),
            rx,
            tx,
        ));

        if let Err(e) = executor.await {
            warn!("Error from executor for handle {handle}: {e:?}");
        }

        self.process_interceptor_response(session_interceptor.await)
            .await;
    }

    /// Returns the number of active sidecar sessions.
    ///
    /// # Returns
    ///
    /// * `usize`: The number of active sessions.
    pub fn active_session_count(&self) -> usize {
        self.session_counter.lock_or_panic().len()
    }

    async fn process_interceptor_response(
        &self,
        result: Result<(HashSet<String>, HashSet<InstanceId>), JoinError>,
    ) {
        match result {
            Ok((sessions, instances)) => {
                for session in sessions {
                    let stop = {
                        let mut counter = self.session_counter.lock_or_panic();
                        if let Entry::Occupied(mut entry) = counter.entry(session.to_owned()) {
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
                        self.stop_session(&session).await;
                    }
                }
                for instance_id in instances {
                    let maybe_session = self
                        .sessions
                        .lock_or_panic()
                        .get(&instance_id.session_id)
                        .cloned();
                    if let Some(session) = maybe_session {
                        session.shutdown_runtime(&instance_id.runtime_id).await;
                    }
                }
            }
            Err(e) => {
                // TODO: APMSP-1076 - Do we need to do more than just log this error?
                debug!("session interceptor encountered an error: {:?}", e);
            }
        }
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
}

impl SidecarInterface for SidecarServer {
    type EnqueueActionsFut = NoResponse;

    fn enqueue_actions(
        self,
        _context: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) -> Self::EnqueueActionsFut {
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

                return no_response();
            }

            let service = entry
                .get()
                .service_name
                .as_deref()
                .unwrap_or("unknown-service");
            let env = entry.get().env.as_deref().unwrap_or("none");

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
            );
            let mut telemetry = telemetry_mutex.lock_or_panic();

            let mut actions_to_process = vec![];
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

        no_response()
    }

    type SetSessionConfigFut = Pin<Box<dyn Send + futures::Future<Output = ()>>>;

    fn set_session_config(
        self,
        _: Context,
        session_id: String,
        #[cfg(unix)] pid: libc::pid_t,
        #[cfg(windows)]
        remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
        config: SessionConfig,
        is_fork: bool,
    ) -> Self::SetSessionConfigFut {
        debug!("Set session config for {session_id} to {config:?}");

        let session = self.get_session(&session_id);
        #[cfg(unix)]
        {
            session.pid.store(pid, Ordering::Relaxed);
        }
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        {
            *session.remote_config_notify_function.lock().unwrap() = remote_config_notify_function;
        }
        *session.remote_config_enabled.lock_or_panic() = config.remote_config_enabled;
        session.modify_telemetry_config(|cfg| {
            cfg.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
            let endpoint =
                get_product_endpoint(ddtelemetry::config::PROD_INTAKE_SUBDOMAIN, &config.endpoint);
            cfg.set_endpoint(endpoint).ok();
            cfg.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
        });
        session.modify_trace_config(|cfg| {
            let endpoint = get_product_endpoint(
                datadog_trace_utils::config_utils::PROD_INTAKE_SUBDOMAIN,
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
            let logs_endpoint = get_product_endpoint(
                datadog_live_debugger::sender::PROD_LOGS_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            let diagnostics_endpoint = get_product_endpoint(
                datadog_live_debugger::sender::PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(logs_endpoint, diagnostics_endpoint).ok();
        });
        if config.endpoint.api_key.is_none() {
            // no agent info if agentless
            *session.agent_infos.lock_or_panic() =
                Some(self.agent_infos.query_for(config.endpoint.clone()));
        }
        session.set_remote_config_invariants(ConfigInvariants {
            language: config.language,
            tracer_version: config.tracer_version,
            endpoint: config.endpoint,
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

        Box::pin(async move {
            if !is_fork {
                session.shutdown_running_instances().await;
            }
            no_response().await
        })
    }

    type ShutdownRuntimeFut = NoResponse;

    fn shutdown_runtime(self, _: Context, instance_id: InstanceId) -> Self::ShutdownRuntimeFut {
        let session = self.get_session(&instance_id.session_id);
        tokio::spawn(async move { session.shutdown_runtime(&instance_id.runtime_id).await });

        no_response()
    }

    type ShutdownSessionFut = NoResponse;

    fn shutdown_session(self, _: Context, session_id: String) -> Self::ShutdownSessionFut {
        tokio::spawn(async move { SidecarServer::stop_session(&self, &session_id).await });
        no_response()
    }

    type SendTraceV04ShmFut = NoResponse;

    fn send_trace_v04_shm(
        self,
        _: Context,
        instance_id: InstanceId,
        handle: ShmHandle,
        _len: usize,
        headers: SerializedTracerHeaderTags,
    ) -> Self::SendTraceV04ShmFut {
        if let Some(endpoint) = self
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            tokio::spawn(async move {
                match handle.map() {
                    Ok(mapped) => {
                        let bytes = tinybytes::Bytes::from(mapped);
                        self.send_trace_v04(&headers, bytes, &endpoint);
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

        no_response()
    }

    type SendTraceV04BytesFut = NoResponse;

    fn send_trace_v04_bytes(
        self,
        _: Context,
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) -> Self::SendTraceV04BytesFut {
        if let Some(endpoint) = self
            .get_session(&instance_id.session_id)
            .get_trace_config()
            .endpoint
            .clone()
        {
            tokio::spawn(async move {
                let bytes = tinybytes::Bytes::from(data);
                self.send_trace_v04(&headers, bytes, &endpoint);
            });
        } else {
            warn!(
                "Received trace data for missing session {}",
                instance_id.session_id
            );
        }

        no_response()
    }

    type SendDebuggerDataShmFut = NoResponse;

    fn send_debugger_data_shm(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        handle: ShmHandle,
        debugger_type: DebuggerType,
    ) -> Self::SendDebuggerDataShmFut {
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

        no_response()
    }

    type SendDebuggerDiagnosticsFut = NoResponse;

    fn send_debugger_diagnostics(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    ) -> Self::SendDebuggerDiagnosticsFut {
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

        no_response()
    }

    type AcquireExceptionHashRateLimiterFut = NoResponse;

    fn acquire_exception_hash_rate_limiter(
        self,
        _: Context,
        exception_hash: u64,
        granularity: Duration,
    ) -> Self::AcquireExceptionHashRateLimiterFut {
        EXCEPTION_HASH_LIMITER
            .lock_or_panic()
            .add(exception_hash, granularity);

        no_response()
    }

    type SetUniversalServiceTagsFut = NoResponse;

    fn set_universal_service_tags(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
    ) -> Self::SetUniversalServiceTagsFut {
        debug!("Registered remote config metadata: instance {instance_id:?}, queue_id: {queue_id:?}, service: {service_name}, env: {env_name}, version: {app_version}");

        let session = self.get_session(&instance_id.session_id);
        #[cfg(windows)]
        #[allow(clippy::unwrap_used)]
        let notify_target = if let Some(handle) = self.process_handle {
            RemoteConfigNotifyTarget {
                process_handle: handle,
                notify_function: *session.remote_config_notify_function.lock().unwrap(),
            }
        } else {
            return no_response();
        };
        #[cfg(unix)]
        let notify_target = RemoteConfigNotifyTarget {
            pid: session.pid.load(Ordering::Relaxed),
        };
        #[allow(clippy::expect_used)]
        let invariants = session
            .get_remote_config_invariants()
            .as_ref()
            .expect("Expecting remote config invariants to be set early")
            .clone();
        let runtime_info = session.get_runtime(&instance_id.runtime_id);
        let mut applications = runtime_info.lock_applications();
        let app = applications.entry(queue_id).or_default();
        if *session.remote_config_enabled.lock_or_panic() {
            app.remote_config_guard = Some(self.remote_configs.add_runtime(
                invariants,
                *session.remote_config_interval.lock_or_panic(),
                instance_id.runtime_id,
                notify_target,
                env_name.clone(),
                service_name.clone(),
                app_version.clone(),
                global_tags.clone(),
            ));
        }
        app.set_metadata(env_name, app_version, service_name, global_tags);

        no_response()
    }

    type SendDogstatsdActionsFut = NoResponse;

    fn send_dogstatsd_actions(
        self,
        _: Context,
        instance_id: InstanceId,
        actions: Vec<DogStatsDActionOwned>,
    ) -> Self::SendDogstatsdActionsFut {
        tokio::spawn(async move {
            self.get_session(&instance_id.session_id)
                .get_dogstatsd()
                .as_ref()
                .inspect(|f| f.send_owned(actions));
        });

        no_response()
    }

    type FlushTracesFut = future::Map<JoinHandle<()>, fn(Result<(), JoinError>)>;

    fn flush_traces(self, _: Context) -> Self::FlushTracesFut {
        let flusher = self.trace_flusher.clone();
        fn report_result(result: Result<(), JoinError>) {
            if let Err(e) = result {
                error!("Failed flushing traces: {e:?}");
            }
        }
        tokio::spawn(async move { flusher.flush().await }).map(report_result)
    }

    type SetTestSessionTokenFut = NoResponse;

    fn set_test_session_token(
        self,
        _: Context,
        session_id: String,
        token: String,
    ) -> Self::SetTestSessionTokenFut {
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

        no_response()
    }

    type PingFut = Ready<()>;

    fn ping(self, _: Context) -> Ready<()> {
        future::ready(())
    }

    type DumpFut = Pin<Box<dyn Send + futures::Future<Output = String>>>;

    fn dump(self, _: Context) -> Self::DumpFut {
        Box::pin(crate::dump::dump())
    }

    type StatsFut = Pin<Box<dyn Send + futures::Future<Output = String>>>;

    fn stats(self, _: Context) -> Self::StatsFut {
        let this = self.clone();
        #[allow(clippy::expect_used)]
        Box::pin(async move {
            let stats = this.compute_stats().await;
            simd_json::serde::to_string(&stats).expect("unable to serialize stats to string")
        })
    }
}

// The session_interceptor function keeps track of session counts and submitted payload counts. It
// also keeps track of RequestIdentifiers and returns hashsets of session and instance ids when the
// rx channel is closed.
async fn session_interceptor(
    session_counter: Arc<Mutex<HashMap<String, u32>>>,
    submitted_payload_count: Arc<AtomicU64>,
    mut rx: tokio::sync::mpsc::Receiver<(
        ServeSidecarInterface<SidecarServer>,
        InFlightRequest<SidecarInterfaceRequest, SidecarInterfaceResponse>,
    )>,
    tx: tokio::sync::mpsc::Sender<(
        ServeSidecarInterface<SidecarServer>,
        InFlightRequest<SidecarInterfaceRequest, SidecarInterfaceResponse>,
    )>,
) -> (HashSet<String>, HashSet<InstanceId>) {
    let mut sessions = HashSet::new();
    let mut instances = HashSet::new();
    loop {
        let (serve, req) = match rx.recv().await {
            None => return (sessions, instances),
            Some(s) => s,
        };

        submitted_payload_count.fetch_add(1, Ordering::Relaxed);

        let instance: RequestIdentifier = req.get().extract_identifier();
        if tx.send((serve, req)).await.is_ok() {
            if let RequestIdentifier::InstanceId(ref instance_id) = instance {
                instances.insert(instance_id.clone());
            }
            if let RequestIdentifier::SessionId(session)
            | RequestIdentifier::InstanceId(InstanceId {
                session_id: session,
                ..
            }) = instance
            {
                if sessions.insert(session.clone()) {
                    match session_counter.lock_or_panic().entry(session) {
                        Entry::Occupied(mut entry) => entry.insert(entry.get() + 1),
                        Entry::Vacant(entry) => *entry.insert(1),
                    };
                }
            }
        }
    }
}

// TODO: APMSP-1079 - Unit tests are sparse for the sidecar server. We should add more.
