// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::{MULTI_LOG_FILTER, MULTI_LOG_WRITER, TemporarilyRetainedMapStats};
use crate::service::{
    ApplicationConfig, ConnectionSessionHandle, InstanceId, SerializedTracerHeaderTags,
    SessionConfig, SidecarAction, SidecarFlushOptions, SidecarInterface,
    application::ActiveApplication,
    connection_session::ConnectionSession,
    dogstatsd_pool::DogStatsDPool,
    sidecar_interface::serve_sidecar_interface_connection,
    telemetry::{TelemetryCachedClient, TelemetryCachedClientSet},
    tracing::TraceFlusher,
};
use libdd_common::{Endpoint, MutexExt};
use libdd_ipc::SeqpacketConn;
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
use libdd_telemetry::metrics::MetricContext;
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerStats};
use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use libdd_trace_utils::span::BytesData;
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::tracer_payload::TraceChunks;
use libdd_trace_utils::tracer_payload::TraceEncoding;
use libdd_trace_utils::tracer_payload::decode_to_trace_chunks;
use manual_future::ManualFutureCompleter;
use std::borrow::Borrow;
use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, trace, warn};

#[cfg(unix)]
use crate::appsec::{AppSecConnection, AppSecManager};
#[cfg(unix)]
use crate::config::AppSecConfig;
use crate::service::agent_info::AgentInfos;
use crate::service::debugger_diagnostics_bookkeeper::{
    DebuggerDiagnosticsBookkeeper, DebuggerDiagnosticsBookkeeperStats,
};
use crate::service::ffe_exposures_flusher;
use crate::service::ffe_flagevaluation_flusher;
use crate::service::ffe_metrics_flusher;
#[cfg(windows)]
use crate::service::remote_configs::RemoteConfigNotifyTarget;
use crate::service::remote_configs::RemoteConfigs;
use crate::service::stats_flusher::{
    ConcentratorKey, SpanConcentratorState, flush_all_stats_now, get_or_create_concentrator,
};
#[cfg(unix)]
use crate::service::telemetry::InProcessTelemetryClientFactory;
use crate::service::tracing::trace_flusher::TraceFlusherStats;
use crate::tracer::ShmLimiters;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_ipc::ipc_server::OwnedServerConn;
use libdd_live_debugger::sender::DebuggerType;
use libdd_remote_config::fetch::MultiTargetStats;
use libdd_tinybytes as tinybytes;
use libdd_trace_utils::tracer_header_tags::{TracerGenericTags, TracerHeaderTags};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SidecarStats {
    trace_flusher: TraceFlusherStats,
    active_connections: u32,
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
/// It holds the state shared across connections: the `TraceFlusher` for sending trace data,
/// telemetry clients, remote config fetchers, and similar aggregators. Session and application
/// state lives on the individual connections.
#[derive(Default, Clone)]
pub struct SidecarServer {
    /// An `Arc` wrapped `TraceFlusher` used for sending trace data.
    pub(crate) trace_flusher: Arc<TraceFlusher>,
    /// Counters of the live connections and their applications.
    connection_stats: Arc<ConnectionStats>,
    /// A `Mutex` guarded `HashMap` that stores the active telemetry clients.
    pub(crate) telemetry_clients: TelemetryCachedClientSet,
    /// A `Mutex` guarded optional `ManualFutureCompleter` for telemetry configuration.
    pub self_telemetry_config:
        Arc<Mutex<Option<ManualFutureCompleter<libdd_telemetry::config::Config>>>>,
    /// Weak references to per-connection payload counters, for telemetry aggregation.
    pub(crate) connection_counters: Arc<Mutex<Vec<Weak<AtomicU64>>>>,
    /// All tracked agent infos per endpoint
    pub agent_infos: AgentInfos,
    /// DogStatsD clients per endpoint
    dogstatsd: DogStatsDPool,
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
    /// AppSec lifecycle and active backend, when supplied by the embedding application.
    #[cfg(unix)]
    appsec: Option<Arc<AppSecManager>>,
    /// Sidecar-owned EVP flagevaluation coalescer, shared across PHP request lifetimes.
    pub(crate) ffe_flagevaluation_coalescer: ffe_flagevaluation_flusher::FlagEvaluationCoalescer,
}

#[derive(Default)]
struct ConnectionStats {
    connections: AtomicU32,
    applications: AtomicU32,
    remote_config_clients: AtomicU32,
}

/// The application slot of a connection, keeping the server-wide counters up to date.
struct ApplicationSlot {
    app: Option<ActiveApplication>,
    stats: Arc<ConnectionStats>,
}

impl ApplicationSlot {
    fn counted(&self) -> (u32, u32) {
        (
            u32::from(self.app.is_some()),
            u32::from(self.app.as_ref().is_some_and(|a| a.has_remote_config())),
        )
    }

    fn modify<R>(&mut self, f: impl FnOnce(&mut Option<ActiveApplication>) -> R) -> R {
        let (apps_before, rc_before) = self.counted();
        let result = f(&mut self.app);
        let (apps_after, rc_after) = self.counted();
        self.stats
            .applications
            .fetch_add(apps_after, Ordering::Relaxed);
        self.stats
            .applications
            .fetch_sub(apps_before, Ordering::Relaxed);
        self.stats
            .remote_config_clients
            .fetch_add(rc_after, Ordering::Relaxed);
        self.stats
            .remote_config_clients
            .fetch_sub(rc_before, Ordering::Relaxed);
        result
    }
}

impl Drop for ApplicationSlot {
    fn drop(&mut self) {
        self.modify(|app| *app = None);
    }
}

/// Per-connection handler, owning all session and application state of its client.
struct ConnectionSidecarHandler {
    server: SidecarServer,
    /// Per-connection counter incremented on each received IPC message.
    submitted_payloads: Arc<AtomicU64>,
    /// Set by set_connection_config.
    session: ConnectionSessionHandle,
    /// The application of the request currently processed by the client.
    application: ApplicationSlot,
    /// All telemetry metric registrations received on this connection, keyed by metric name.
    /// Used to auto-register metrics in newly-created telemetry clients when a metric point
    /// for a previously registered metric arrives for a new (service, env) combination.
    metric_registrations: HashMap<String, MetricContext>,
    /// The connection this handler serves.
    connection: OwnedServerConn,
    /// The AppSec client of this connection.
    #[cfg(unix)]
    appsec: AppSecConnection,
}

impl ConnectionSidecarHandler {
    fn new(server: SidecarServer, connection: OwnedServerConn) -> Self {
        let submitted_payloads = Arc::new(AtomicU64::new(0));
        server
            .connection_counters
            .lock_or_panic()
            .push(Arc::downgrade(&submitted_payloads));
        server
            .connection_stats
            .connections
            .fetch_add(1, Ordering::Relaxed);
        let application = ApplicationSlot {
            app: None,
            stats: server.connection_stats.clone(),
        };
        let session = ConnectionSessionHandle::default();
        Self {
            server,
            submitted_payloads,
            #[cfg(unix)]
            appsec: AppSecConnection::new(session.clone()),
            session,
            application,
            metric_registrations: Default::default(),
            connection,
        }
    }

    fn application_identity(&self) -> Option<(String, String)> {
        self.application
            .app
            .as_ref()
            .map(|app| (app.config.service_name.clone(), app.config.env_name.clone()))
    }

    /// Processes actions for the given (service, env), or the current application if `None`.
    fn process_actions(&self, target: Option<(String, String)>, actions: Vec<SidecarAction>) {
        let Some(session) = self.session.load() else {
            info!("Dropping actions sent on a connection without configuration");
            return;
        };
        let connection_metric_registrations = self.metric_registrations.clone();
        let trace_config = &session.tracer_config;
        let runtime_metadata = session.runtime_metadata();
        let instance_id = &session.instance_id;

        let ffe_http_client = self.server.ffe_http_client.clone();
        let actions: Vec<SidecarAction> = actions
            .into_iter()
            .filter_map(|a| match a {
                SidecarAction::FfeExposureBatch(batch) => {
                    if let Some(base) = trace_config.endpoint.as_ref() {
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
                    None
                }
                SidecarAction::FfeFlagEvaluationBatch(batch) => {
                    if let Some(base) = trace_config.endpoint.as_ref() {
                        if let Some(ep) = ffe_flagevaluation_flusher::flagevaluation_endpoint(base)
                        {
                            self.server.ffe_flagevaluation_coalescer.enqueue(
                                ffe_http_client.clone(),
                                ep,
                                batch,
                                ffe_flagevaluation_flusher::evp_origin_from_language(
                                    &trace_config.language,
                                ),
                                trace_config.tracer_version.as_str(),
                            );
                        } else {
                            debug!(
                                "ffe_flagevaluation_flusher: could not derive endpoint, dropping batch"
                            );
                        }
                    } else {
                        debug!(
                            "ffe_flagevaluation_flusher: no session endpoint, dropping batch"
                        );
                    }
                    None
                }
                SidecarAction::FfeEvaluationMetrics { context, metrics } => {
                    if let Some(ep) = session.otlp_metrics_endpoint.clone() {
                        let client = ffe_http_client.clone();
                        let context = context.clone();
                        let metrics = metrics.clone();
                        tokio::spawn(async move {
                            ffe_metrics_flusher::send_metrics(&client, &ep, context, metrics).await;
                        });
                    } else {
                        debug!("ffe_metrics_flusher: no configured endpoint, dropping batch");
                    }
                    None
                }
                action => Some(action),
            })
            .collect();

        if actions.is_empty() {
            return;
        }

        let Some((service, env)) = target.or_else(|| self.application_identity()) else {
            info!("No application found for instance {instance_id:?}");
            return;
        };
        let (service, env) = (service.as_str(), env.as_str());

        {
            let process_tags = session.process_tags_with_svc_source();
            let session_config = &session.telemetry_config;

            // A client can retire between lookup and locking it. Retry while retaining the
            // guard used to check for Some, so Stop cannot slip between checking and enqueueing.
            let mut telemetry_mutex = self.server.telemetry_clients.get_or_create(
                service,
                env,
                instance_id,
                &runtime_metadata,
                || session_config.clone(),
                process_tags.clone(),
            );
            let mut telemetry_guard = telemetry_mutex.lock_or_panic();
            while telemetry_guard.is_none() {
                drop(telemetry_guard);
                telemetry_mutex = self.server.telemetry_clients.get_or_create(
                    service,
                    env,
                    instance_id,
                    &runtime_metadata,
                    || session_config.clone(),
                    process_tags.clone(),
                );
                telemetry_guard = telemetry_mutex.lock_or_panic();
            }
            let Some(telemetry) = telemetry_guard.as_mut() else {
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
                    "Buffered telemetry info changed for instance {instance_id:?} and {service}/{env}"
                );
                telemetry.write_shm_file();
            }

            if !actions_to_process.is_empty() {
                // Queued sends only retain worker handles, so Stop can retire the cached
                // client without discarding earlier batches that have yet to run.
                let processed = telemetry.process_actions(actions_to_process);
                let worker = telemetry.worker.clone();
                let last_handle = telemetry.handle.take();
                telemetry.handle = Some(tokio::spawn(async move {
                    if let Some(last_handle) = last_handle {
                        last_handle.await.ok();
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

            // Mark the client retired before unlocking it, so another batch cannot be
            // queued behind Stop while this client is still in the cache.
            let retired_client = if remove_client {
                telemetry_guard.take()
            } else {
                None
            };

            // Stats and flush lock the cache before individual clients. Release the client
            // guard before acquiring the cache to preserve that lock order.
            drop(telemetry_guard);

            if remove_client {
                self.server.telemetry_clients.remove_telemetry_client(
                    service,
                    env,
                    &telemetry_mutex,
                );
                drop(retired_client);
                info!("Removing telemetry client for instance {instance_id:?}");
            }
        }
    }

    fn update_session(&self, what: &str, f: impl FnOnce(&mut ConnectionSession)) {
        if !self.session.update(f) {
            debug!("Ignoring {what} for a connection without configuration");
        }
    }
}

impl Drop for ConnectionSidecarHandler {
    fn drop(&mut self) {
        // Let the appsec helper drop the client of this connection.
        #[cfg(unix)]
        if let Some(appsec) = self.server.appsec.as_ref() {
            appsec.disconnect(&mut self.appsec);
        }
        self.server
            .connection_stats
            .connections
            .fetch_sub(1, Ordering::Relaxed);
    }
}

impl SidecarServer {
    pub(crate) fn shm_limiters(&self) -> &ShmLimiters {
        self.remote_configs.shm_limiters()
    }

    #[cfg(unix)]
    pub(crate) fn with_appsec_telemetry(
        mut self,
        telemetry: InProcessTelemetryClientFactory,
    ) -> Self {
        self.appsec = Some(Arc::new(AppSecManager::new(telemetry)));
        self
    }

    #[cfg(unix)]
    pub(crate) async fn ensure_appsec_started(&self, config: &AppSecConfig) -> bool {
        let Some(appsec) = self.appsec.as_ref() else {
            error!("AppSec is unavailable: no lifecycle manager was configured");
            return false;
        };
        appsec.ensure_started(config).await
    }

    #[cfg(unix)]
    pub(crate) async fn shutdown_appsec(&self) {
        if let Some(appsec) = self.appsec.as_ref() {
            appsec.shutdown().await;
        }
    }

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
        serve_sidecar_interface_connection(&mut ConnectionSidecarHandler::new(self, server_conn))
            .await;
    }

    /// Returns the number of open connections.
    pub fn active_connection_count(&self) -> usize {
        self.connection_stats.connections.load(Ordering::Relaxed) as usize
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
                error!(
                    "Failed to convert SerializedTracerHeaderTags into TracerHeaderTags with error {:?}",
                    e
                );
                return;
            }
        };
        self.send_trace(headers, data, target, retry_interval, TraceEncoding::V04)
    }

    /// Entry point for the V1 trace path. Input bytes are a V1 msgpack `TracerPayload` from the
    /// SDK; the [`TraceEncoding::V1`] tag drives [`decode_to_trace_chunks`] to the V1 decoder,
    /// and [`SendData`] then re-encodes the same shape as V1 on the wire to the agent.
    ///
    /// Unlike `send_trace_v04`, only the generic bool/int flags plus `lang_interpreter`/
    /// `lang_vendor` (which have no equivalent in the V1 payload model) cross the IPC boundary —
    /// no full `TracerHeaderTags`/`SerializedTracerHeaderTags` envelope. The remaining fields
    /// (`lang`, `lang_version`, `tracer_version`, `container_id`) are read back out of the
    /// decoded V1 payload once it's available below and still forwarded to the agent as
    /// `Datadog-Meta-*`/`Datadog-Container-Id` headers: the agent relies on those headers, not
    /// just the payload body, to identify the source of malformed payloads or misbehaving
    /// tracers when it can't decode the body, and populating them only on the agent side would
    /// leave a gap whenever a tracer is upgraded ahead of its agent.
    ///
    /// `target` is `tracer::Config::endpoint_v1`, already normalized to the agent's
    /// `/v1.0/traces` route (or the shared intake URL for agentless sessions) by
    /// `tracer::Config::set_endpoint`.
    fn send_trace_v1(
        &self,
        generic: TracerGenericTags,
        lang_interpreter: &str,
        lang_vendor: &str,
        data: tinybytes::Bytes,
        target: &Endpoint,
        retry_interval: u64,
    ) {
        match decode_to_trace_chunks(data, TraceEncoding::V1) {
            Ok((payload, size)) => {
                let TraceChunks::V1(tracer_payload) = &payload else {
                    unreachable!(
                        "decode_to_trace_chunks(_, TraceEncoding::V1) always returns TraceChunks::V1"
                    );
                };
                // Cheap refcounted clones: decouples the header strings from `payload`'s
                // borrow so `payload` can still be moved into `enqueue_trace` below.
                let lang = tracer_payload.language_name.clone();
                let lang_version = tracer_payload.language_version.clone();
                let tracer_version = tracer_payload.tracer_version.clone();
                let container_id = tracer_payload.container_id.clone();

                let headers = TracerHeaderTags {
                    lang: lang.borrow(),
                    lang_version: lang_version.borrow(),
                    lang_interpreter,
                    lang_vendor,
                    tracer_version: tracer_version.borrow(),
                    container_id: container_id.borrow(),
                    generic,
                };
                debug!(
                    "Received {} bytes of data for {:?} with headers {:?}",
                    size, target, headers
                );
                trace!("Parsed the trace payload and enqueuing it for sending: {payload:?}");
                self.enqueue_trace(payload, size, headers, target, retry_interval);
            }
            Err(e) => {
                error!(
                    "Failed to collect trace chunks from msgpack with error {:?}",
                    e
                )
            }
        }
    }

    fn send_trace(
        &self,
        headers: TracerHeaderTags,
        data: tinybytes::Bytes,
        target: &Endpoint,
        retry_interval: u64,
        encoding: TraceEncoding,
    ) {
        debug!(
            "Received {} bytes of data for {:?} with headers {:?}",
            data.len(),
            target,
            headers
        );

        match decode_to_trace_chunks(data, encoding) {
            Ok((payload, size)) => {
                trace!("Parsed the trace payload and enqueuing it for sending: {payload:?}");
                self.enqueue_trace(payload, size, headers, target, retry_interval);
            }
            Err(e) => {
                error!(
                    "Failed to collect trace chunks from msgpack with error {:?}",
                    e
                )
            }
        }
    }

    fn enqueue_trace(
        &self,
        payload: TraceChunks<BytesData>,
        size: usize,
        headers: TracerHeaderTags,
        target: &Endpoint,
        retry_interval: u64,
    ) {
        let mut data = SendData::new(
            size,
            payload.into_tracer_payload_collection(),
            headers,
            target,
        );
        let strategy = RetryStrategy::new(5, retry_interval, RetryBackoffType::Exponential, None);
        data.set_retry_strategy(strategy);
        self.trace_flusher.enqueue(data);
    }

    pub async fn compute_stats(&self) -> SidecarStats {
        let (futures, metric_counts, active_telemetry_clients): (Vec<_>, Vec<_>, u32) = {
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

            (
                futures,
                metric_counts,
                clients.len().try_into().unwrap_or(u32::MAX),
            )
        };

        let telemetry_stats = futures::future::join_all(futures).await;
        let telemetry_stats_errors = telemetry_stats.iter().filter(|r| r.is_err()).count() as u32;
        let connection_stats = &self.connection_stats;

        SidecarStats {
            trace_flusher: self.trace_flusher.stats(),
            active_connections: connection_stats.connections.load(Ordering::Relaxed),
            active_telemetry_clients,
            active_apps: connection_stats.applications.load(Ordering::Relaxed),
            remote_config_clients: connection_stats
                .remote_config_clients
                .load(Ordering::Relaxed),
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

    fn connection(&self) -> &OwnedServerConn {
        &self.connection
    }

    async fn enter_crashtracker_receiver(&mut self) {
        #[cfg(unix)]
        crate::crashtracker::run_crashtracker_receiver(
            self.connection.async_conn(),
            self.connection.peer().pid,
        )
        .await;
    }

    async fn enqueue_actions(&mut self, actions: Vec<SidecarAction>) {
        self.process_actions(None, actions);
    }

    async fn enqueue_actions_for_service(
        &mut self,
        service_name: String,
        env_name: String,
        actions: Vec<SidecarAction>,
    ) {
        self.process_actions(Some((service_name, env_name)), actions);
    }

    async fn register_telemetry_metric(&mut self, metric: MetricContext) {
        self.metric_registrations
            .entry(metric.name.clone())
            .or_insert(metric);
    }

    async fn set_connection_config(
        &mut self,
        instance_id: InstanceId,
        #[cfg(windows)] remote_config_notify_target: Option<RemoteConfigNotifyTarget>,
        config: SessionConfig,
    ) {
        debug!("Set connection config for {instance_id:?} to {config:?}");

        self.server.trace_flusher.interval_ms.store(
            u64::try_from(config.flush_interval.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.server.trace_flusher.min_force_flush_size_bytes.store(
            u32::try_from(config.force_flush_size).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        self.server.trace_flusher.min_force_drop_size_bytes.store(
            u32::try_from(config.force_drop_size).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );

        let session = ConnectionSession::from_config(
            instance_id,
            config,
            #[cfg(unix)]
            self.connection.peer().pid,
            #[cfg(windows)]
            remote_config_notify_target,
            &self.server.agent_infos,
            &self.server.dogstatsd,
        );

        if let Some(completer) = self.server.self_telemetry_config.lock_or_panic().take() {
            let config = session.telemetry_config.clone();
            tokio::spawn(async move {
                completer.complete(config).await;
            });
        }

        self.session.store(session);
        // The application is registered for the previous session: the client sends it again.
        self.application.modify(|app| *app = None);
    }

    async fn set_process_tags(&mut self, process_tags: Vec<Tag>) {
        self.update_session("process tags", |session| {
            session.set_process_tags(process_tags)
        });

        // The remote config target is derived from the process tags.
        let Some(session) = self.session.load() else {
            return;
        };
        let Some(notify_target) = session.notify_target() else {
            return;
        };
        self.application.modify(|app| {
            if let Some(app) = app {
                app.update_remote_config(&self.server.remote_configs, &session, 0, notify_target);
            }
        });
    }

    async fn set_user_service_defined(&mut self, is_defined: bool) {
        self.update_session("user service flag", |session| {
            session.set_user_service_defined(is_defined)
        });
    }

    async fn set_application(
        &mut self,
        application: Option<ApplicationConfig>,
        remote_config_generation: u64,
    ) {
        let session = self.session.load();
        self.application.modify(|current| {
            let Some(config) = application else {
                if current.take().is_some() {
                    debug!(
                        "Cleared the application of {:?}",
                        session.map(|s| s.instance_id.clone())
                    );
                }
                return;
            };
            debug!(
                "Registered application metadata: instance {:?}, service: {}, env: {}, version: {}",
                session.as_ref().map(|s| &s.instance_id),
                config.service_name,
                config.env_name,
                config.app_version
            );
            let app = match current {
                Some(app) if app.config == config => return,
                Some(app) => {
                    app.update(config);
                    app
                }
                None => current.insert(ActiveApplication::new(config)),
            };
            if let Some(session) = session {
                if let Some(notify_target) = session.notify_target() {
                    app.update_remote_config(
                        &self.server.remote_configs,
                        &session,
                        remote_config_generation,
                        notify_target,
                    );
                }
            }
        });
    }

    async fn send_trace_v04_shm(
        &mut self,
        handle: ShmHandle,
        _len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        let Some(session) = self.session.load() else {
            warn!("Received trace data on a connection without configuration");
            return;
        };
        let trace_config = &session.tracer_config;
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
                session.instance_id.session_id
            );
        }
    }

    async fn send_trace_v04_bytes(&mut self, data: Vec<u8>, headers: SerializedTracerHeaderTags) {
        let Some(session) = self.session.load() else {
            warn!("Received trace data on a connection without configuration");
            return;
        };
        let trace_config = &session.tracer_config;

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
                session.instance_id.session_id
            );
        }
    }

    async fn send_trace_v1_shm(
        &mut self,
        handle: ShmHandle,
        _len: usize,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    ) {
        let Some(session) = self.session.load() else {
            warn!("Received trace data on a connection without configuration");
            return;
        };
        let trace_config = &session.tracer_config;
        if let Some(endpoint) = trace_config.endpoint_v1.clone() {
            let server = self.server.clone();
            let retry_interval = trace_config.retry_interval;
            tokio::spawn(async move {
                match handle.map() {
                    Ok(mapped) => {
                        let bytes = tinybytes::Bytes::from(mapped);
                        server.send_trace_v1(
                            generic,
                            &lang_interpreter,
                            &lang_vendor,
                            bytes,
                            &endpoint,
                            retry_interval,
                        );
                    }
                    Err(e) => error!("Failed mapping shared trace data memory: {}", e),
                }
            });
        } else {
            warn!(
                "Received trace data ({handle:?}) for missing session {}",
                session.instance_id.session_id
            );
        }
    }

    async fn send_trace_v1_bytes(
        &mut self,
        data: Vec<u8>,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    ) {
        let Some(session) = self.session.load() else {
            warn!("Received trace data on a connection without configuration");
            return;
        };
        let trace_config = &session.tracer_config;

        if let Some(endpoint) = trace_config.endpoint_v1.clone() {
            let server = self.server.clone();
            let retry_interval = trace_config.retry_interval;
            tokio::spawn(async move {
                let bytes = tinybytes::Bytes::from(data);
                server.send_trace_v1(
                    generic,
                    &lang_interpreter,
                    &lang_vendor,
                    bytes,
                    &endpoint,
                    retry_interval,
                );
            });
        } else {
            warn!(
                "Received trace data for missing session {}",
                session.instance_id.session_id
            );
        }
    }

    async fn send_debugger_data_shm(&mut self, handle: ShmHandle, debugger_type: DebuggerType) {
        let Some(session) = self.session.load() else {
            warn!("Received live debugger data on a connection without configuration");
            return;
        };
        match handle.map() {
            Ok(mapped) => {
                if let Some(app) = self.application.app.as_mut() {
                    session.send_debugger_data(app, debugger_type, mapped);
                } else {
                    warn!(
                        "No application for {:?} - skipping live debugger data",
                        session.instance_id
                    );
                }
            }
            Err(e) => error!("Failed mapping shared debugger data memory: {}", e),
        }
    }

    async fn send_debugger_diagnostics(&mut self, diagnostics_payload: Vec<u8>) {
        let Some(session) = self.session.load() else {
            warn!("Received live debugger diagnostics on a connection without configuration");
            return;
        };
        let payload = match serde_json::from_slice(diagnostics_payload.as_slice()) {
            Ok(payload) => payload,
            Err(e) => {
                error!("Received invalid live debugger diagnostics: {e}");
                return;
            }
        };
        // We segregate RC by endpoint.
        // So we assume that runtime ids are unique per endpoint and we can safely filter globally.
        if self
            .server
            .debugger_diagnostics_bookkeeper
            .add_payload(&payload)
        {
            if let Some(app) = self.application.app.as_mut() {
                #[allow(clippy::unwrap_used)]
                session.send_debugger_data(
                    app,
                    DebuggerType::Diagnostics,
                    serde_json::to_vec(&vec![payload]).unwrap(),
                );
            } else {
                warn!(
                    "No application for {:?} - skipping live debugger diagnostics",
                    session.instance_id
                );
            }
        }
    }

    async fn acquire_exception_hash_rate_limiter(
        &mut self,
        exception_hash: u64,
        granularity: Duration,
    ) {
        if let Some(limiter) = &self.server.shm_limiters().exceptions {
            limiter.lock_or_panic().add(exception_hash, granularity);
        }
    }

    async fn send_dogstatsd_actions(&mut self, actions: Vec<DogStatsDActionOwned>) {
        let Some(client) = self.session.load().and_then(|s| s.dogstatsd.clone()) else {
            return;
        };
        // The first send may resolve the endpoint, keep that off the connection.
        tokio::spawn(async move { client.send_owned(actions) });
    }

    async fn add_span_to_concentrator(
        &mut self,
        env: String,
        version: String,
        span: libdd_ipc::shm_stats::OwnedShmSpanInput,
    ) {
        let Some(session) = self.session.load() else {
            return;
        };
        let session_id = session.instance_id.session_id.as_str();
        // Lazily create the concentrator on first IPC span for this (env, version, service).
        // The session id is intentionally passed as the stats runtime_id: all processes of a
        // session (e.g. php-fpm workers) then report as one runtime, which keeps the backend
        // cardinality of client-side stats low.
        if let Some(state) = get_or_create_concentrator(
            &self.server.span_concentrators,
            &self.server.telemetry_clients,
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

    async fn flush(&mut self, options: SidecarFlushOptions) {
        let flag_evaluations = options.flag_evaluations;
        let ffe_coalescer = self.server.ffe_flagevaluation_coalescer.clone();
        let ffe_http_client = self.server.ffe_http_client.clone();
        let flush_flag_evaluations = async move {
            if flag_evaluations {
                ffe_coalescer.flush_now(ffe_http_client).await;
            }
        };

        let traces_and_stats = options.traces_and_stats;
        let flusher = self.server.trace_flusher.clone();
        let span_concentrators = self.server.span_concentrators.clone();
        let flush_traces_and_stats = async move {
            if traces_and_stats {
                if let Err(e) = tokio::spawn(async move { flusher.flush().await }).await {
                    error!("Failed flushing traces: {e:?}");
                }
                flush_all_stats_now(&span_concentrators).await;
                debug!("Finished executing flush() for traces and stats")
            }
        };

        tokio::join!(flush_flag_evaluations, flush_traces_and_stats);

        if options.telemetry {
            let workers: Vec<_> = {
                let clients = self.server.telemetry_clients.inner.lock_or_panic();
                clients
                    .values()
                    .filter_map(|entry| {
                        entry
                            .client
                            .lock_or_panic()
                            .as_ref()
                            .map(|c| c.worker.clone())
                    })
                    .collect()
            };
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

    async fn flush_signal(
        &mut self,
        options: SidecarFlushOptions,
        completion: libdd_ipc::platform::PlatformHandle<std::io::PipeWriter>,
    ) {
        self.flush(options).await;
        drop(completion);
    }

    async fn set_test_session_token(&mut self, token: String) {
        let token = if token.is_empty() {
            None
        } else {
            Some(Cow::Owned(token))
        };
        debug!("Update test token of the connection to {token:?}");
        self.update_session("test session token", |session| {
            session.set_test_session_token(token)
        });
    }

    async fn ping(&mut self) {}

    async fn dump(&mut self) -> String {
        crate::dump::dump().await
    }

    async fn stats(&mut self) -> String {
        let stats = self.server.compute_stats().await;
        #[allow(clippy::expect_used)]
        simd_json::serde::to_string(&stats).expect("unable to serialize stats to string")
    }

    async fn ensure_appsec_started(&mut self, log_file_path: Vec<u8>, log_level: String) -> bool {
        #[cfg(unix)]
        {
            let config = AppSecConfig {
                log_file_path: std::ffi::OsString::from_vec(log_file_path),
                log_level,
            };
            self.server.ensure_appsec_started(&config).await
        }
        #[cfg(not(unix))]
        {
            _ = (log_file_path, log_level);
            false
        }
    }

    async fn send_appsec_message(&mut self, data: Vec<u8>) -> (Vec<u8>, bool) {
        #[cfg(unix)]
        {
            let Some(appsec) = self.server.appsec.as_ref() else {
                warn!("appsec: no backend is available");
                return (vec![], true /* disconnect */);
            };

            if self.session.load().is_none() {
                warn!(
                    "appsec: extension has sent an appsec message on a connection \
                     without configuration"
                );
                return (vec![], true /* disconnect */);
            }

            let Some(response) = appsec.send_message(&mut self.appsec, data).await else {
                info!("appsec: not in running phase anymore");
                return (vec![], true /* disconnect */);
            };
            (response.data, response.disconnect)
        }
        #[cfg(not(unix))]
        {
            _ = data;
            (vec![], false)
        }
    }
}

#[cfg(all(test, unix))]
#[path = "flagevaluation_privacy_tests.rs"]
mod flagevaluation_privacy_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{
        DynamicInstrumentationConfigState, FfeEvaluationMetric, FfeExposure, FfeExposureBatch,
        FfeFlagEvaluationBatch, FfeFlagEvaluationEvent, FfeTelemetryContext, FlagKey,
        RuntimeMetadata,
    };
    use httpmock::{Method::POST, MockServer};
    use libdd_ffe::telemetry::flagevaluation::EVP_FLAGEVALUATION_PATH;
    use libdd_ipc::PeerCredentials;
    use libdd_telemetry::config::Config;
    use tokio::time::{Duration as TokioDuration, sleep};

    /// Build a handler backed by a throwaway socketpair connection. These tests call the handler
    /// methods directly and never read the connection, but the handler requires one.
    fn test_handler(server: SidecarServer) -> ConnectionSidecarHandler {
        let (local, peer) = SeqpacketConn::socketpair().expect("socketpair");
        drop(peer);
        let conn = OwnedServerConn::new(local).expect("OwnedServerConn");
        ConnectionSidecarHandler::new(server, conn)
    }

    fn test_application(service_name: &str) -> ApplicationConfig {
        ApplicationConfig {
            service_name: service_name.to_owned(),
            env_name: "none".to_owned(),
            app_version: String::new(),
            global_tags: vec![],
            dynamic_instrumentation_state: DynamicInstrumentationConfigState::NotSet,
        }
    }

    /// Sets the application whose telemetry goes to the client keyed
    /// ("unknown-service", "none").
    fn set_test_application(handler: &mut ConnectionSidecarHandler) {
        handler
            .application
            .modify(|app| *app = Some(ActiveApplication::new(test_application("unknown-service"))));
    }

    fn configure_session(
        handler: &ConnectionSidecarHandler,
        instance_id: InstanceId,
        configure: impl FnOnce(&mut ConnectionSession),
    ) {
        let mut session = ConnectionSession::for_test(instance_id);
        configure(&mut session);
        handler.session.store(session);
    }

    fn set_trace_endpoint(session: &mut ConnectionSession, url: String) {
        session
            .tracer_config
            .set_endpoint(Endpoint {
                url: url.parse().unwrap(),
                ..Endpoint::default()
            })
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg_attr(miri, ignore)]
    async fn stop_releases_client_before_cache_removal() {
        use libdd_telemetry::data::{Configuration, ConfigurationOrigin};
        use std::time::Instant;

        let server = SidecarServer::default();
        let mut handler = test_handler(server.clone());
        configure_session(&handler, InstanceId::new("session", "runtime"), |_| {});
        set_test_application(&mut handler);

        // This batch stays queued on the current-thread runtime until the final await.
        // Both actions must still be delivered after Stop retires the cached client.
        let (stats_tx, stats_rx) = futures::channel::oneshot::channel();
        handler
            .enqueue_actions(vec![
                SidecarAction::Telemetry(TelemetryActions::AddConfig(Configuration {
                    name: "pending-config".to_owned(),
                    value: Some("value".to_owned()),
                    origin: ConfigurationOrigin::Code,
                    config_id: None,
                    seq_id: None,
                })),
                SidecarAction::Telemetry(TelemetryActions::CollectStats(stats_tx)),
            ])
            .await;

        let key = ("unknown-service".to_owned(), "none".to_owned());
        let client = server.telemetry_clients.inner.lock_or_panic()[&key]
            .client
            .clone();
        let retired_entry = {
            let client_guard = client.lock_or_panic();
            let references_before_stop = Arc::strong_count(&client);
            let runtime = tokio::runtime::Handle::current();
            let stop = std::thread::spawn(move || {
                runtime.block_on(handler.enqueue_actions(vec![SidecarAction::Telemetry(
                    TelemetryActions::Lifecycle(LifecycleAction::Stop),
                )]));
            });

            // Wait until Stop has looked up the client, then hold the cache as stats/flush do.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Arc::strong_count(&client) == references_before_stop && Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            let stop_started = Arc::strong_count(&client) > references_before_stop;
            let mut cache_guard = server.telemetry_clients.inner.lock_or_panic();
            drop(client_guard);

            let deadline = Instant::now() + Duration::from_secs(5);
            let retired_before_removal = loop {
                if let Ok(guard) = client.try_lock() {
                    if guard.is_none() {
                        break true;
                    }
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::yield_now();
            };

            let retired_entry = cache_guard.remove(&key).unwrap();
            // Release the contended lock before asserting, so regressions fail without deadlocking.
            drop(cache_guard);
            stop.join().unwrap();
            assert!(stop_started, "Stop never looked up the cached client");
            assert!(
                retired_before_removal,
                "Stop left the client locked or usable while waiting for the cache"
            );
            assert!(
                !server
                    .telemetry_clients
                    .inner
                    .lock_or_panic()
                    .contains_key(&key)
            );

            retired_entry
        };
        assert!(client.lock_or_panic().is_none());

        // Restore the empty cache entry to deterministically exercise a lookup between
        // retirement and cache removal, before letting the delayed removal run again.
        server
            .telemetry_clients
            .inner
            .lock_or_panic()
            .insert(key.clone(), retired_entry);
        let next_instance = InstanceId::new("session", "next-runtime");
        let replacement = server.telemetry_clients.get_or_create(
            &key.0,
            &key.1,
            &next_instance,
            &RuntimeMetadata::new("php", "8.3", "1.0"),
            Config::default,
            Vec::new(),
        );
        assert!(!Arc::ptr_eq(&client, &replacement));
        assert!(replacement.lock_or_panic().is_some());
        server
            .telemetry_clients
            .remove_telemetry_client(&key.0, &key.1, &client);
        assert!(Arc::ptr_eq(
            &server.telemetry_clients.inner.lock_or_panic()[&key].client,
            &replacement,
        ));

        let mut next_handler = test_handler(server.clone());
        configure_session(&next_handler, next_instance, |_| {});
        set_test_application(&mut next_handler);
        let (next_stats_tx, next_stats_rx) = futures::channel::oneshot::channel();
        next_handler
            .enqueue_actions(vec![
                SidecarAction::Telemetry(TelemetryActions::AddConfig(Configuration {
                    name: "replacement-config".to_owned(),
                    value: Some("value".to_owned()),
                    origin: ConfigurationOrigin::Code,
                    config_id: None,
                    seq_id: None,
                })),
                SidecarAction::Telemetry(TelemetryActions::CollectStats(next_stats_tx)),
            ])
            .await;

        let stats = tokio::time::timeout(Duration::from_secs(5), stats_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.configurations_stored, 1);
        let next_stats = tokio::time::timeout(Duration::from_secs(5), next_stats_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next_stats.configurations_stored, 1);
    }

    /// Connections of the same instance (e.g. threads of one process) each hold their own
    /// application, so closing one of them must not drop the application of the others.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn cleanup_keeps_applications_of_other_connections_of_the_instance() {
        let server = SidecarServer::default();
        let instance_id = InstanceId::new("session", "runtime");
        let mut closing = test_handler(server.clone());
        let mut remaining = test_handler(server.clone());
        configure_session(&closing, instance_id.clone(), |_| {});
        configure_session(&remaining, instance_id, |_| {});
        closing
            .set_application(Some(test_application("closing-service")), 0)
            .await;
        remaining
            .set_application(Some(test_application("remaining-service")), 0)
            .await;
        assert_eq!(server.compute_stats().await.active_apps, 2);

        drop(closing);

        assert_eq!(
            remaining
                .application
                .app
                .as_ref()
                .map(|app| app.config.clone()),
            Some(test_application("remaining-service"))
        );
        assert_eq!(server.compute_stats().await.active_apps, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn new_connection_config_drops_the_application_of_the_previous_session() {
        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |_| {});
        handler
            .set_application(Some(test_application("service")), 0)
            .await;
        assert!(handler.application.app.is_some());

        handler
            .set_connection_config(
                InstanceId::new("session", "child"),
                SessionConfig::for_test(),
            )
            .await;
        assert!(handler.application.app.is_none());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn application_without_remote_config_is_kept_until_cleared() {
        let server = SidecarServer::default();
        let (local, _peer) = SeqpacketConn::socketpair().unwrap();
        // A live peer, so that the session has a target for remote config notifications.
        let conn = OwnedServerConn::from_async(
            local.into_async_conn().unwrap(),
            PeerCredentials {
                pid: std::process::id(),
                uid: 0,
                gid: 0,
            },
        );
        let mut handler = ConnectionSidecarHandler::new(server.clone(), conn);
        handler
            .set_connection_config(
                InstanceId::new("session", "runtime"),
                #[cfg(windows)]
                None,
                SessionConfig {
                    remote_config_enabled: false,
                    ..SessionConfig::for_test()
                },
            )
            .await;
        #[cfg(unix)]
        assert!(handler.session.load().unwrap().notify_target().is_some());

        handler
            .set_application(Some(test_application("svc")), 0)
            .await;
        {
            let slot = &handler.application;
            let app = slot.app.as_ref().expect("the application is kept");
            assert_eq!(app.config, test_application("svc"));
            assert!(!app.has_remote_config());
        }
        let stats = server.compute_stats().await;
        assert_eq!(stats.active_apps, 1);
        assert_eq!(stats.remote_config_clients, 0);

        handler.set_application(None, 0).await;
        assert!(handler.application.app.is_none());
        let stats = server.compute_stats().await;
        assert_eq!(stats.active_apps, 0);
        assert_eq!(stats.remote_config_clients, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore = "requires native IPC sockets")]
    async fn signal_flush_preserves_normal_reply_accounting() {
        use super::super::blocking::SidecarTransport;
        use libdd_ipc::platform::PlatformHandle;
        use std::io::Read;

        let (client, server) = SeqpacketConn::socketpair().unwrap();
        let server = tokio::spawn(SidecarServer::default().accept_connection(server));
        tokio::task::spawn_blocking(move || {
            let mut transport = SidecarTransport::from(client);
            let sender = &mut transport.inner;
            let (mut receiver, completion) = std::io::pipe().unwrap();
            for _ in 0..18 {
                assert!(
                    sender
                        .channel
                        .try_send_set_test_session_token("test".to_owned())
                );
            }
            assert!(sender.channel.try_send_flush_signal(
                SidecarFlushOptions::default(),
                PlatformHandle::from(completion),
            ));
            assert_eq!(receiver.read(&mut [0]).unwrap(), 0);
            sender.channel.call_ping().unwrap();
            assert_eq!(sender.channel.0.outstanding(), 0);
            sender.channel.call_dump().unwrap();
        })
        .await
        .unwrap();
        // Socketpair peer-close detection differs by platform; this test owns the server task.
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled());
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    async fn raw_signal_flush_preserves_normal_reply_accounting() {
        use super::super::{blocking::SidecarTransport, signal_flush::SignalFlush};
        let (client, server) = SeqpacketConn::socketpair().unwrap();
        let server = tokio::spawn(SidecarServer::default().accept_connection(server));
        tokio::task::spawn_blocking(move || {
            let mut transport = SidecarTransport::from(client);
            let flush =
                SignalFlush::prepare(&mut transport, SidecarFlushOptions::default()).unwrap();
            let sender = &mut transport.inner;
            // Leave ordinary ACKs pending on the original connection when the raw request arrives.
            for _ in 0..18 {
                assert!(
                    sender
                        .channel
                        .try_send_set_test_session_token("test".to_owned())
                );
            }
            assert_eq!(unsafe { flush.run() }, 0);
            sender.channel.call_ping().unwrap();
            assert_eq!(sender.channel.0.outstanding(), 0);
            // A typed reply would fail to decode if the emergency request had left a stray ACK.
            sender.channel.call_dump().unwrap();
            drop(flush);
            drop(transport);
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
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
            serial_id: None,
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

    fn ffe_flag_evaluation_batch() -> FfeFlagEvaluationBatch {
        FfeFlagEvaluationBatch {
            context: ffe_context(),
            flag_evaluations: vec![FfeFlagEvaluationEvent {
                timestamp: 123,
                flag: FlagKey {
                    key: "flag".to_owned(),
                },
                first_evaluation: 100,
                last_evaluation: 123,
                evaluation_count: 1,
                variant: None,
                allocation: None,
                targeting_rule: None,
                targeting_key: None,
                context: None,
                error: None,
                runtime_default_used: false,
                observe_full_evaluation_data: false,
                is_degraded: false,
                field_omissions: Default::default(),
            }],
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

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
        });

        assert!(handler.application.app.is_none());

        handler
            .enqueue_actions(vec![SidecarAction::FfeExposureBatch(FfeExposureBatch {
                context: ffe_context(),
                exposures: vec![ffe_exposure("user")],
            })])
            .await;

        for _ in 0..100 {
            if exposures_mock.calls_async().await == 1 {
                break;
            }
            sleep(TokioDuration::from_millis(10)).await;
        }

        exposures_mock.assert_async().await;
        assert!(handler.application.app.is_none());
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

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            session.otlp_metrics_endpoint = Some(Endpoint {
                url: http_server.url("/v1/metrics").parse().unwrap(),
                test_token: Some(test_session_token.into()),
                ..Endpoint::default()
            });
        });

        assert!(handler.application.app.is_none());

        handler
            .enqueue_actions(vec![SidecarAction::FfeEvaluationMetrics {
                context: ffe_context(),
                metrics: vec![ffe_metric()],
            }])
            .await;

        for _ in 0..100 {
            if metrics_mock.calls_async().await == 1 {
                break;
            }
            sleep(TokioDuration::from_millis(10)).await;
        }

        metrics_mock.assert_async().await;
        assert!(handler.application.app.is_none());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn mixed_flag_evaluation_and_telemetry_actions_reach_their_consumers() {
        let http_server = MockServer::start_async().await;
        let evaluations = http_server
            .mock_async(|when, then| {
                when.method(POST).path(EVP_FLAGEVALUATION_PATH);
                then.status(202);
            })
            .await;
        let mut handler = test_handler(SidecarServer::default());
        configure_session(
            &handler,
            InstanceId::new("mixed-ffe-session", "runtime"),
            |session| set_trace_endpoint(session, http_server.url("/")),
        );
        set_test_application(&mut handler);
        let (before_tx, before_rx) = futures::channel::oneshot::channel();
        let (after_tx, after_rx) = futures::channel::oneshot::channel();
        handler
            .enqueue_actions(vec![
                SidecarAction::Telemetry(TelemetryActions::CollectStats(before_tx)),
                SidecarAction::FfeFlagEvaluationBatch(ffe_flag_evaluation_batch()),
                SidecarAction::Telemetry(TelemetryActions::CollectStats(after_tx)),
            ])
            .await;
        // Neither neighboring action may be dropped or routed to the FFE coalescer.
        tokio::time::timeout(TokioDuration::from_secs(5), async {
            before_rx.await.unwrap();
            after_rx.await.unwrap();
        })
        .await
        .unwrap();
        handler
            .flush(SidecarFlushOptions {
                flag_evaluations: true,
                ..SidecarFlushOptions::default()
            })
            .await;
        evaluations.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn flush_options_control_ffe_flag_evaluations() {
        let http_server = MockServer::start_async().await;
        let flag_evaluations_mock = http_server
            .mock_async(|when, then| {
                when.method(POST).path(EVP_FLAGEVALUATION_PATH);
                then.status(202);
            })
            .await;

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
            session.tracer_config.tracer_version = "9.9.9".to_owned();
        });

        handler
            .enqueue_actions(vec![SidecarAction::FfeFlagEvaluationBatch(
                ffe_flag_evaluation_batch(),
            )])
            .await;

        handler.flush(SidecarFlushOptions::default()).await;
        sleep(TokioDuration::from_millis(50)).await;
        assert_eq!(flag_evaluations_mock.calls_async().await, 0);

        handler
            .flush(SidecarFlushOptions {
                flag_evaluations: true,
                ..SidecarFlushOptions::default()
            })
            .await;

        flag_evaluations_mock.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn flag_evaluations_use_originating_tracer_identity() {
        let http_server = MockServer::start_async().await;
        let flag_evaluations_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header("DD-EVP-ORIGIN", "dd-trace-py")
                    .header("DD-EVP-ORIGIN-VERSION", "9.9.9");
                then.status(202);
            })
            .await;

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
            session.tracer_config.language = "python".to_owned();
            session.tracer_config.tracer_version = "9.9.9".to_owned();
        });

        handler
            .enqueue_actions(vec![SidecarAction::FfeFlagEvaluationBatch(
                ffe_flag_evaluation_batch(),
            )])
            .await;
        handler
            .flush(SidecarFlushOptions {
                flag_evaluations: true,
                ..SidecarFlushOptions::default()
            })
            .await;

        flag_evaluations_mock.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn flag_evaluations_omit_origin_when_tracer_language_is_blank() {
        let http_server = MockServer::start_async().await;
        let flag_evaluations_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header_missing("DD-EVP-ORIGIN")
                    .header("DD-EVP-ORIGIN-VERSION", "9.9.9");
                then.status(202);
            })
            .await;

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
            session.tracer_config.language = " \t".to_owned();
            session.tracer_config.tracer_version = "9.9.9".to_owned();
        });

        handler
            .enqueue_actions(vec![SidecarAction::FfeFlagEvaluationBatch(
                ffe_flag_evaluation_batch(),
            )])
            .await;
        handler
            .flush(SidecarFlushOptions {
                flag_evaluations: true,
                ..SidecarFlushOptions::default()
            })
            .await;

        flag_evaluations_mock.assert_calls_async(1).await;
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

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
        });
        set_test_application(&mut handler);

        assert!(handler.application.app.is_some());

        handler.enqueue_actions(Vec::new()).await;

        sleep(TokioDuration::from_millis(50)).await;

        assert_eq!(exposures_mock.calls_async().await, 0);
        assert_eq!(metrics_mock.calls_async().await, 0);
    }

    fn sample_v1_trace_payload_bytes() -> Vec<u8> {
        use libdd_tinybytes::BytesString;
        use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
        use libdd_trace_utils::span::v1::{Span as V1Span, TraceChunkBytes, TracerPayloadBytes};

        fn bs(s: &str) -> BytesString {
            BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
        }

        let span = V1Span {
            service: bs("svc"),
            name: bs("GET /users"),
            resource: bs("/users"),
            span_id: 42,
            start: 1_700_000_000_000,
            duration: 1_500,
            ..Default::default()
        };

        let chunk = TraceChunkBytes {
            trace_id: [1u8; 16],
            spans: vec![span],
            ..Default::default()
        };

        let payload = TracerPayloadBytes {
            language_name: bs("rust"),
            language_version: bs("1.87"),
            tracer_version: bs("9.9.9"),
            chunks: vec![chunk],
            ..Default::default()
        };

        to_vec_from_v1(&payload)
    }

    /// Agentful sessions have their trace endpoint normalized to `/v0.4/traces` by
    /// `tracer::Config::set_endpoint` since it doesn't know ahead of time which encoding will be
    /// used. This exercises the full `send_trace_v1_bytes` chain to ensure V1 payloads are
    /// redirected to the agent's `/v1.0/traces` route instead, and that `lang_interpreter`/
    /// `lang_vendor` (which the V1 payload model has no room for) survive as headers, alongside
    /// `lang`/`lang_version`/`tracer_version` (which are read back out of the decoded V1 payload
    /// — see `send_trace_v1`).
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn send_trace_v1_bytes_routes_to_v1_endpoint() {
        let http_server = MockServer::start_async().await;
        let v1_mock = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1.0/traces")
                    .header("datadog-meta-lang-interpreter", "cpython")
                    .header("datadog-meta-lang-interpreter-vendor", "cpython-vendor")
                    .header("datadog-meta-lang", "rust")
                    .header("datadog-meta-lang-version", "1.87")
                    .header("datadog-meta-tracer-version", "9.9.9");
                then.status(200);
            })
            .await;
        let v04_mock = http_server
            .mock_async(|when, then| {
                when.method(POST).path("/v0.4/traces");
                then.status(200);
            })
            .await;

        let mut handler = test_handler(SidecarServer::default());
        configure_session(&handler, InstanceId::new("session", "runtime"), |session| {
            set_trace_endpoint(session, http_server.url("/"));
        });

        handler
            .send_trace_v1_bytes(
                sample_v1_trace_payload_bytes(),
                TracerGenericTags::default(),
                "cpython".to_owned(),
                "cpython-vendor".to_owned(),
            )
            .await;

        // send_trace_v1_bytes spawns the actual send, so give it a chance to enqueue before
        // forcing the flusher to flush and join.
        sleep(TokioDuration::from_millis(50)).await;
        handler.server.trace_flusher.join().await.unwrap();

        v1_mock.assert_async().await;
        assert_eq!(v04_mock.calls_async().await, 0);
    }
}

// TODO: APMSP-1079 - Unit tests are sparse for the sidecar server. We should add more.
