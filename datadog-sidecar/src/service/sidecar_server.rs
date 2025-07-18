// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::log::{TemporarilyRetainedMapStats, MULTI_LOG_FILTER, MULTI_LOG_WRITER};
use crate::service::{
    sidecar_interface::ServeSidecarInterface,
    telemetry::{AppInstance, AppOrQueue},
    tracing::TraceFlusher,
    EnqueuedTelemetryData, InstanceId, QueueId, RequestIdentification, RequestIdentifier,
    RuntimeInfo, RuntimeMetadata, SerializedTracerHeaderTags, SessionConfig, SessionInfo,
    SidecarAction, SidecarInterface, SidecarInterfaceRequest, SidecarInterfaceResponse,
};
use datadog_ipc::platform::{AsyncChannel, ShmHandle};
use datadog_ipc::tarpc;
use datadog_ipc::tarpc::context::Context;
use datadog_ipc::transport::Transport;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::tracer_payload::decode_to_trace_chunks;
use datadog_trace_utils::tracer_payload::TraceEncoding;
use ddcommon::{Endpoint, MutexExt};
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerStats,
};
use futures::future;
use futures::future::{join_all, Ready};
use manual_future::{ManualFuture, ManualFutureCompleter};
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
use crate::service::runtime_info::ActiveApplication;
use crate::service::telemetry::enqueued_telemetry_stats::EnqueuedTelemetryStats;
use crate::service::tracing::trace_flusher::TraceFlusherStats;
use datadog_ipc::platform::FileBackedHandle;
use datadog_ipc::tarpc::server::{Channel, InFlightRequest};
use datadog_live_debugger::sender::DebuggerType;
use datadog_remote_config::fetch::{ConfigInvariants, MultiTargetStats};
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use ddcommon::tag::Tag;
use dogstatsd_client::{new, DogStatsDActionOwned};
use tinybytes;

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
    apps: u32,
    active_apps: u32,
    enqueued_apps: u32,
    enqueued_telemetry_data: EnqueuedTelemetryStats,
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
        let mut telemetry_stats_errors = 0;
        let telemetry_stats = join_all({
            let sessions = self.sessions.lock_or_panic();
            let mut futures = vec![];
            for (_, s) in sessions.iter() {
                let runtimes = s.lock_runtimes();
                for (_, r) in runtimes.iter() {
                    let apps = r.lock_apps();
                    for (_, a) in apps.iter() {
                        if let Some(Some(existing_app)) = a.peek() {
                            match existing_app.telemetry.stats() {
                                Ok(future) => futures.push(future),
                                Err(_) => telemetry_stats_errors += 1,
                            }
                        }
                    }
                }
            }
            futures
        })
        .await;
        let sessions = self.sessions.lock_or_panic();
        SidecarStats {
            trace_flusher: self.trace_flusher.stats(),
            sessions: sessions.len() as u32,
            session_counter_size: self.session_counter.lock_or_panic().len() as u32,
            runtimes: sessions
                .values()
                .map(|s| s.lock_runtimes().len() as u32)
                .sum(),
            apps: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| r.lock_apps().len() as u32)
                        .sum::<u32>()
                })
                .sum(),
            active_apps: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| r.lock_applications().len() as u32)
                        .sum::<u32>()
                })
                .sum(),
            enqueued_apps: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| {
                            r.lock_applications()
                                .values()
                                .filter(|a| matches!(a.app_or_actions, AppOrQueue::Queue(_)))
                                .count() as u32
                        })
                        .sum::<u32>()
                })
                .sum(),
            enqueued_telemetry_data: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| {
                            r.lock_applications()
                                .values()
                                .filter_map(|a| match &a.app_or_actions {
                                    AppOrQueue::Queue(q) => Some(q.stats()),
                                    _ => None,
                                })
                                .sum()
                        })
                        .sum()
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
            telemetry_metrics_contexts: sessions
                .values()
                .map(|s| {
                    s.lock_runtimes()
                        .values()
                        .map(|r| {
                            r.lock_apps()
                                .values()
                                .map(|a| {
                                    a.peek().unwrap_or(&None).as_ref().map_or(0, |w| {
                                        w.telemetry_metrics.lock_or_panic().len() as u32
                                    })
                                })
                                .sum::<u32>()
                        })
                        .sum::<u32>()
                })
                .sum(),
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
        fn is_stop_actions(actions: &[SidecarAction]) -> bool {
            actions.len() == 1
                && matches!(
                    actions[0],
                    SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop))
                )
        }

        let rt_info = self.get_runtime(&instance_id);
        let mut applications = rt_info.lock_applications();
        match applications.entry(queue_id) {
            Entry::Occupied(mut entry) => {
                let value = entry.get_mut();
                match value.app_or_actions {
                    AppOrQueue::Inactive => {
                        if is_stop_actions(&actions) {
                            entry.remove();
                        } else {
                            value.app_or_actions =
                                AppOrQueue::Queue(EnqueuedTelemetryData::processed(actions));
                        }
                    }
                    AppOrQueue::Queue(ref mut data) => {
                        data.process(actions);
                    }
                    AppOrQueue::App(ref service_future) => {
                        let service_future = service_future.clone();
                        // drop on stop
                        if actions.iter().any(|action| {
                            matches!(
                                action,
                                SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                                    LifecycleAction::Stop
                                ))
                            )
                        }) {
                            entry.remove();
                        }
                        let apps = rt_info.apps.clone();
                        tokio::spawn(async move {
                            let service = service_future.await;
                            let app_future = if let Some(fut) = apps.lock_or_panic().get(&service) {
                                fut.clone()
                            } else {
                                return;
                            };
                            if let Some(mut app) = app_future.await {
                                let actions =
                                    EnqueuedTelemetryData::process_immediately(actions, &mut app)
                                        .await;
                                app.telemetry.send_msgs(actions).await.ok();
                            }
                        });
                    }
                }
            }
            Entry::Vacant(entry) => {
                if !is_stop_actions(&actions) {
                    entry.insert(ActiveApplication {
                        app_or_actions: AppOrQueue::Queue(EnqueuedTelemetryData::processed(
                            actions,
                        )),
                        ..Default::default()
                    });
                }
            }
        }

        no_response()
    }

    type RegisterServiceAndFlushQueuedActionsFut = NoResponse;
    fn register_service_and_flush_queued_actions(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        runtime_meta: RuntimeMetadata,
        service_name: String,
        env_name: String,
    ) -> Self::RegisterServiceAndFlushQueuedActionsFut {
        // We need a channel to have enqueuing code await
        let (future, completer) = ManualFuture::new();
        let app_or_queue = {
            let rt_info = self.get_runtime(&instance_id);
            let mut applications = rt_info.lock_applications();
            match applications.get_mut(&queue_id) {
                Some(ActiveApplication {
                    app_or_actions: ref mut app @ AppOrQueue::Queue(_),
                    ..
                }) => Some(std::mem::replace(app, AppOrQueue::App(future.shared()))),
                None
                | Some(ActiveApplication {
                    app_or_actions: AppOrQueue::Inactive,
                    ..
                }) => Some(AppOrQueue::Queue(EnqueuedTelemetryData::default())),
                _ => None,
            }
        };
        if let Some(AppOrQueue::Queue(mut enqueued_data)) = app_or_queue {
            let rt_info = self.get_runtime(&instance_id);
            let manual_app_future = rt_info.get_app(&service_name, &env_name);

            let instance_future = if manual_app_future.completer.is_some() {
                let mut builder = TelemetryWorkerBuilder::new_fetch_host(
                    service_name.to_owned(),
                    runtime_meta.language_name.to_owned(),
                    runtime_meta.language_version.to_owned(),
                    runtime_meta.tracer_version.to_owned(),
                );
                builder.runtime_id = Some(instance_id.runtime_id.to_owned());
                builder.application.env = Some(env_name.to_owned());
                let session_info = self.get_session(&instance_id.session_id);
                let mut config = session_info
                    .session_config
                    .lock_or_panic()
                    .clone()
                    .unwrap_or_else(ddtelemetry::config::Config::from_env);
                config.restartable = true;
                builder.config = config.clone();
                Some(builder.spawn().map(move |result| {
                    if result.is_ok() {
                        info!("spawning telemetry worker {config:?}");
                    }
                    result
                }))
            } else {
                None
            };

            tokio::spawn(async move {
                if let Some(instance_future) = instance_future {
                    let instance_option = match instance_future.await {
                        Ok((handle, worker_join)) => {
                            let instance = AppInstance {
                                telemetry: handle,
                                telemetry_worker_shutdown: worker_join
                                    .map(Result::ok)
                                    .boxed()
                                    .shared(),
                                telemetry_metrics: Default::default(),
                            };

                            let mut actions: Vec<TelemetryActions> = vec![];
                            enqueued_data.extract_telemetry_actions(&mut actions).await;
                            instance.telemetry.send_msgs(actions).await.ok();

                            instance
                                .telemetry
                                .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
                                .await
                                .ok();
                            Some(instance)
                        }
                        Err(e) => {
                            error!("could not spawn telemetry worker {:?}", e);
                            None
                        }
                    };

                    #[allow(clippy::expect_used)]
                    manual_app_future
                        .completer
                        .expect("Completed expected Some ManualFuture for application instance, but found none")
                        .complete(instance_option)
                        .await;
                }

                if let Some(mut app) = manual_app_future.app_future.await {
                    // Register metrics
                    for metric in std::mem::take(&mut enqueued_data.metrics).into_iter() {
                        app.register_metric(metric);
                    }

                    let mut actions: Vec<_> = std::mem::take(&mut enqueued_data.actions);

                    // Send metric points
                    for point in std::mem::take(&mut enqueued_data.points) {
                        actions.push(app.to_telemetry_point(point));
                    }

                    // drop on stop
                    if actions.iter().any(|action| {
                        matches!(action, TelemetryActions::Lifecycle(LifecycleAction::Stop))
                    }) {
                        // Avoid self.get_runtime(), it could create a new one.
                        if let Some(session) =
                            self.sessions.lock_or_panic().get(&instance_id.session_id)
                        {
                            if let Some(runtime) =
                                session.lock_runtimes().get(&instance_id.runtime_id)
                            {
                                runtime.lock_applications().remove(&queue_id);
                            }
                        }
                    }

                    app.telemetry.send_msgs(actions).await.ok();

                    let mut extracted_actions: Vec<TelemetryActions> = vec![];
                    enqueued_data
                        .extract_telemetry_actions(&mut extracted_actions)
                        .await;
                    app.telemetry.send_msgs(extracted_actions).await.ok();

                    // Ok, we dequeued all messages, now new enqueue_actions calls can handle it
                    completer.complete((service_name, env_name)).await;
                }
            });
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

    type SetRemoteConfigDataFut = NoResponse;

    fn set_remote_config_data(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
    ) -> Self::SetRemoteConfigDataFut {
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
        app.remote_config_guard = Some(self.remote_configs.add_runtime(
            invariants,
            *session.remote_config_interval.lock_or_panic(),
            instance_id.runtime_id,
            notify_target,
            env_name.clone(),
            service_name,
            app_version.clone(),
            global_tags.clone(),
        ));
        app.set_metadata(env_name, app_version, global_tags);

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
        #[allow(clippy::expect_used)]
        Box::pin(async move {
            let stats = self.compute_stats().await;
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
