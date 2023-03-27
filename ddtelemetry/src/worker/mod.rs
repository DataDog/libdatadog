// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

mod builder;
mod http_client;

use crate::{
    config::{self, ProvideConfig},
    metrics::{ContextKey, MetricBuckets, MetricContexts},
    DEFAULT_API_VERSION,
};

use self::builder::ConfigBuilder;

use super::{
    data::{self, Application, Dependency, Host, Integration, Log, Telemetry},
    Config,
};

use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::{Arc, Condvar, Mutex},
    time,
};

use anyhow::Result;
use futures::future::{self};
use http::Request;

use serde::{Deserialize, Serialize};
use tokio::{
    runtime::{self, Handle},
    sync::mpsc,
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

fn time_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .unwrap_or_default()
        .as_secs_f64()
}

use ddcommon::tag::Tag;

macro_rules! telemetry_worker_log {
    ($worker:expr , ERROR , $fmt_str:tt, $($arg:tt)*) => {
        #[cfg(feature = "tracing")]
        tracing::error!($fmt_str, $($arg)*);
        if $worker.config.is_telemetry_debug_logging_enabled() {
            eprintln!(concat!("{}: Telemetry worker ERROR: ", $fmt_str), time_now(), $($arg)*);
        }
    };
    ($worker:expr , DEBUG , $fmt_str:tt, $($arg:tt)*) => {
        #[cfg(feature = "tracing")]
        tracing::debug!($fmt_str, $($arg)*);
        if $worker.config.is_telemetry_debug_logging_enabled() {
            println!(concat!("{}: Telemetry worker DEBUG: ", $fmt_str), time_now(), $($arg)*);
        }
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub enum TelemetryActions {
    AddPoint((f64, ContextKey, Vec<Tag>)),
    FlushMetricAggregate,
    SendMetrics,
    AddConfig((String, String)),
    AddDependecy(Dependency),
    SendDependencies,

    AddIntegration(Integration),
    SendIntegrations,

    AddLog((LogIdentifier, Log)),
    SendLogs,

    Start,
    Stop,
    Heartbeat,
}

/// Identifies a logging location uniquely
///
/// The identifier is a single 64 bit integer to save space an memory
/// and to be able to generic on the way different languages handle
///
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogIdentifier {
    // Collisions? Never heard of them
    indentifier: u64,
}

#[derive(Debug)]
struct UnfluhsedLogEntry {
    number_skipped: u32,
    log: Log,
}

// Holds the current state of the telemetry worker
struct TelemetryWorkerData {
    started: bool,
    library_config: Vec<(String, String)>,
    unflushed_integrations: Vec<Integration>,
    unflushed_dependencies: Vec<Dependency>,
    unflushed_logs: HashMap<LogIdentifier, UnfluhsedLogEntry>,
    metric_contexts: MetricContexts,
    metric_buckets: MetricBuckets,
    host: Host,
    app: Application,
}

pub struct TelemetryWorker {
    config: Config,
    mailbox: mpsc::Receiver<TelemetryActions>,
    cancellation_token: CancellationToken,
    seq_id: u64,
    runtime_id: String,
    client: Box<dyn http_client::HttpClient + Sync + Send>,
    deadlines: Scheduler,
    data: TelemetryWorkerData,
}

impl TelemetryWorker {
    fn handle_result(&self, result: &Result<()>) {
        if let Err(err) = result {
            telemetry_worker_log!(self, ERROR, "{}", err);
        } else {
            telemetry_worker_log!(self, DEBUG, "Request successfully request",);
        }
    }

    async fn recv_next_action(&mut self) -> TelemetryActions {
        self.deadlines.recv_next_action(&mut self.mailbox).await
    }

    // Runs a state machine that waits for actions, either from the worker's
    // mailbox, or scheduled actions from the worker's deadline object.
    async fn run(mut self) {
        use TelemetryActions::*;

        loop {
            if self.cancellation_token.is_cancelled() {
                return;
            }

            let action = self.recv_next_action().await;

            tracing::info!("handling action {action:?}");

            telemetry_worker_log!(self, DEBUG, "Handling action {:?}", action);
            match action {
                Start => {
                    if !self.data.started {
                        let req = self.build_app_started();
                        self.send_request(req).await;
                        self.deadlines.schedule_next_heartbeat();
                        self.data.started = true;
                    }
                }
                AddDependecy(dep) => {
                    self.data.unflushed_dependencies.push(dep);
                    if self.data.started {
                        self.deadlines.schedule_next_send_dependency();
                    }
                }
                AddIntegration(integration) => {
                    self.data.unflushed_integrations.push(integration);
                    if self.data.started {
                        self.deadlines.schedule_next_send_integration();
                    }
                }
                AddLog((entry, log)) => {
                    self.data
                        .unflushed_logs
                        .entry(entry)
                        .and_modify(|e| e.number_skipped += 1)
                        .or_insert(UnfluhsedLogEntry {
                            number_skipped: 0,
                            log,
                        });
                    if self.data.started {
                        self.deadlines.schedule_next_send_logs();
                    }
                }
                SendDependencies => self.flush_deps().await,
                SendIntegrations => self.flush_intgs().await,
                SendLogs => self.flush_logs().await,
                Heartbeat => {
                    if self.data.started {
                        let req = self.build_heartbeat();
                        self.send_request(req).await;
                    }
                    self.deadlines.schedule_next_heartbeat();
                }
                Stop => {
                    if !self.data.started {
                        return;
                    }
                    self.data.metric_buckets.flush_agregates();
                    let requests = IntoIterator::into_iter([
                        self.build_app_stop(),
                        self.build_integrations_change(),
                        self.build_dependencies_loaded(),
                    ])
                    .chain(self.build_metrics_series())
                    .map(|r| self.send_request(r));
                    future::join_all(requests).await;

                    return;
                }
                AddPoint((point, key, extra_tags)) => {
                    self.deadlines.schedule_next_send_metrics();
                    self.data.metric_buckets.add_point(
                        key,
                        &self.data.metric_contexts,
                        point,
                        extra_tags,
                    )
                }
                FlushMetricAggregate => {
                    self.data.metric_buckets.flush_agregates();
                    self.deadlines.flush_aggreg_done();
                }
                SendMetrics => {
                    if let Some(req) = self.build_metrics_series() {
                        self.send_request(req).await;
                    };
                    self.deadlines.send_metrics_done();
                }
                AddConfig(cfg) => self.data.library_config.push(cfg),
            }
        }
    }

    async fn flush_deps(&mut self) {
        if self.data.unflushed_dependencies.is_empty() {
            return;
        }
        let req = self.build_dependencies_loaded();
        self.send_request(req).await;
        self.deadlines.send_dependency_done();
    }

    async fn flush_intgs(&mut self) {
        if self.data.unflushed_integrations.is_empty() {
            return;
        }

        let req = self.build_integrations_change();
        self.send_request(req).await;
        self.deadlines.send_integrations_done();
    }

    async fn flush_logs(&mut self) {
        if self.data.unflushed_logs.is_empty() {
            return;
        }

        let req = self.build_logs();
        self.send_request(req).await;
        self.deadlines.send_logs_done();
    }

    fn build_heartbeat(&mut self) -> Result<Request<hyper::Body>> {
        self.build_request(data::Payload::AppHearbeat(()))
    }

    fn build_metrics_series(&mut self) -> Option<Result<Request<hyper::Body>>> {
        let mut series = Vec::new();
        for (context_key, extra_tags, points) in self.data.metric_buckets.flush_series() {
            let context_guard = self.data.metric_contexts.get_context(context_key);
            let maybe_context = context_guard.read();
            let context = match maybe_context {
                Some(context) => context,
                None => {
                    telemetry_worker_log!(
                        self,
                        ERROR,
                        "Context not found for key {:?}",
                        context_key
                    );
                    continue;
                }
            };

            let mut tags = extra_tags;
            tags.extend(context.tags.iter().cloned());
            series.push(data::metrics::Serie {
                namespace: context.namespace,
                metric: context.name.clone(),
                tags,
                points,
                common: context.common,
                _type: context.metric_type,
            });
        }
        if series.is_empty() {
            return None;
        }

        Some(
            self.build_request(data::Payload::GenerateMetrics(data::GenerateMetrics {
                series,
            })),
        )
    }

    fn build_app_stop(&mut self) -> Result<Request<hyper::Body>> {
        self.build_request(data::Payload::AppClosing(()))
    }

    fn build_app_started(&mut self) -> Result<Request<hyper::Body>> {
        let app_started = data::AppStarted {
            integrations: std::mem::take(&mut self.data.unflushed_integrations),
            dependencies: std::mem::take(&mut self.data.unflushed_dependencies),
            config: std::mem::take(&mut self.data.library_config),
        };
        self.build_request(data::Payload::AppStarted(app_started))
    }

    fn build_dependencies_loaded(&mut self) -> Result<Request<hyper::Body>> {
        let deps_loaded = data::Payload::AppDependenciesLoaded(data::AppDependenciesLoaded {
            dependencies: std::mem::take(&mut self.data.unflushed_dependencies),
        });

        self.build_request(deps_loaded)
    }

    fn build_integrations_change(&mut self) -> Result<Request<hyper::Body>> {
        let integrations_change =
            data::Payload::AppIntegrationsChange(data::AppIntegrationsChange {
                integrations: std::mem::take(&mut self.data.unflushed_integrations),
            });
        self.build_request(integrations_change)
    }

    fn build_logs(&mut self) -> Result<Request<hyper::Body>> {
        let logs = self
            .data
            .unflushed_logs
            .drain()
            .map(|(_, mut e)| {
                use std::fmt::Write;
                if e.number_skipped > 0 {
                    write!(
                        &mut e.log.message,
                        "\nSkipped {} messages",
                        e.number_skipped
                    )
                    .unwrap();
                }
                e.log
            })
            .collect();
        self.build_request(data::Payload::Logs(logs))
    }

    fn next_seq_id(&mut self) -> u64 {
        self.seq_id += 1;
        self.seq_id
    }

    fn build_request(&mut self, payload: data::Payload) -> Result<Request<hyper::Body>> {
        let seq_id = self.next_seq_id();
        let tel = Telemetry {
            api_version: DEFAULT_API_VERSION,
            tracer_time: time::SystemTime::now()
                .duration_since(time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            runtime_id: &self.runtime_id,
            seq_id,
            host: &self.data.host,
            application: &self.data.app,
            payload,
        };

        telemetry_worker_log!(self, DEBUG, "Prepared payload: {:?}", tel);

        let req = self
            .config
            .into_request_builder()?
            .method(http::Method::POST)
            .header(http::header::CONTENT_TYPE, "application/json");

        let body = hyper::Body::from(serde_json::to_vec(&tel)?);
        Ok(req.body(body)?)
    }

    async fn send_request(&self, req: Result<Request<hyper::Body>>) {
        let res = (|| async {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    Err(anyhow::anyhow!("Request cancelled"))
                },
                r = self.client.request(req?) => {
                    match r {
                        Ok(_) => {
                            Ok(())
                        }
                        Err(e) => Err(e.into()),
                    }
                }
            }
        })()
        .await;
        self.handle_result(&res);
    }
}

struct InnerTelemetryShutdown {
    is_shutdown: Mutex<bool>,
    condvar: Condvar,
}

impl InnerTelemetryShutdown {
    fn wait_for_shutdown(&self) {
        drop(
            self.condvar
                .wait_while(self.is_shutdown.lock().unwrap(), |is_shutdown| {
                    !*is_shutdown
                })
                .unwrap(),
        )
    }

    fn shutdown_finished(&self) {
        *self.is_shutdown.lock().unwrap() = true;
        self.condvar.notify_all();
    }
}

#[derive(Clone)]
pub struct TelemetryWorkerHandle {
    sender: mpsc::Sender<TelemetryActions>,
    shutdown: Arc<InnerTelemetryShutdown>,
    cancellation_token: CancellationToken,
    runtime: runtime::Handle,
    contexts: MetricContexts,
}

impl TelemetryWorkerHandle {
    pub fn register_metric_context(
        &self,
        name: String,
        tags: Vec<Tag>,
        metric_type: data::metrics::MetricType,
        common: bool,
        namespace: data::metrics::MetricNamespace,
    ) -> ContextKey {
        self.contexts
            .register_metric_context(name, tags, metric_type, common, namespace)
    }

    pub fn try_send_msg(&self, msg: TelemetryActions) -> Result<()> {
        Ok(self.sender.try_send(msg)?)
    }

    pub async fn send_msg(&self, msg: TelemetryActions) -> Result<()> {
        Ok(self.sender.send(msg).await?)
    }

    pub async fn send_msgs<T>(&self, msgs: T) -> Result<()>
    where
        T: IntoIterator<Item = TelemetryActions>,
    {
        for msg in msgs {
            self.sender.send(msg).await?;
        }

        Ok(())
    }

    pub async fn send_msg_timeout(
        &self,
        msg: TelemetryActions,
        timeout: time::Duration,
    ) -> Result<()> {
        Ok(self.sender.send_timeout(msg, timeout).await?)
    }

    pub fn send_start(&self) -> Result<()> {
        Ok(self.sender.try_send(TelemetryActions::Start)?)
    }

    pub fn send_stop(&self) -> Result<()> {
        Ok(self.sender.try_send(TelemetryActions::Stop)?)
    }

    pub fn cancel_requests_with_deadline(&self, deadline: Instant) {
        let token = self.cancellation_token.clone();
        let f = async move {
            tokio::time::sleep_until(deadline).await;
            token.cancel()
        };
        self.runtime.spawn(f);
    }

    pub fn add_dependency(&self, name: String, version: Option<String>) -> Result<()> {
        self.sender
            .try_send(TelemetryActions::AddDependecy(Dependency {
                name,
                version,
                hash: None,
            }))?;
        Ok(())
    }

    pub fn add_integration(
        &self,
        name: String,
        version: Option<String>,
        compatible: Option<bool>,
        enabled: Option<bool>,
        auto_enabled: Option<bool>,
    ) -> Result<()> {
        self.sender
            .try_send(TelemetryActions::AddIntegration(Integration {
                name,
                version,
                compatible,
                enabled,
                auto_enabled,
            }))?;
        Ok(())
    }

    pub fn add_log<T: Hash>(
        &self,
        identifier: T,
        message: String,
        level: data::LogLevel,
        stack_trace: Option<String>,
    ) -> Result<()> {
        let mut hasher = DefaultHasher::new();
        identifier.hash(&mut hasher);
        self.sender.try_send(TelemetryActions::AddLog((
            LogIdentifier {
                indentifier: hasher.finish(),
            },
            data::Log {
                message,
                level,
                stack_trace,
            },
        )))?;
        Ok(())
    }

    pub fn add_point(&self, value: f64, context: &ContextKey, extra_tags: Vec<Tag>) -> Result<()> {
        self.sender
            .try_send(TelemetryActions::AddPoint((value, *context, extra_tags)))?;
        Ok(())
    }

    pub fn wait_for_shutdown(&self) {
        self.shutdown.wait_for_shutdown();
    }
}

pub struct TelemetryWorkerBuilder {
    pub host: Host,
    pub application: Application,
    pub runtime_id: Option<String>,
    pub library_config: Vec<(String, String)>,
    pub native_deps: bool,
    pub rust_shared_lib_deps: bool,
    pub config: builder::ConfigBuilder,
}

impl TelemetryWorkerBuilder {
    pub fn new_fetch_host(
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> Self {
        Self {
            host: crate::build_host(),
            application: Application {
                service_name,
                language_name,
                language_version,
                tracer_version,
                ..Default::default()
            },
            runtime_id: None,
            library_config: Vec::new(),
            native_deps: true,
            rust_shared_lib_deps: false,
            config: ConfigBuilder::default(),
        }
    }

    pub fn new(
        hostname: String,
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> Self {
        Self {
            host: Host {
                hostname,
                ..Default::default()
            },
            application: Application {
                service_name,
                language_name,
                language_version,
                tracer_version,
                ..Default::default()
            },
            runtime_id: None,
            library_config: Vec::new(),
            native_deps: true,
            rust_shared_lib_deps: false,
            config: ConfigBuilder::default(),
        }
    }

    fn gather_deps(&self) -> Vec<Dependency> {
        Vec::new() // Dummy dependencies
    }

    fn build_worker(
        self,
        external_config: Config,
        tokio_runtime: Handle,
    ) -> Result<(TelemetryWorkerHandle, TelemetryWorker)> {
        let (tx, mailbox) = mpsc::channel(5000);
        let shutdown = Arc::new(InnerTelemetryShutdown {
            is_shutdown: Mutex::new(false),
            condvar: Condvar::new(),
        });

        let contexts = MetricContexts::default();
        let token = CancellationToken::new();
        let unflushed_dependencies = self.gather_deps();
        let config = self.config.merge(external_config);
        let client = http_client::from_config(&config);
        let worker = TelemetryWorker {
            data: TelemetryWorkerData {
                started: false,
                library_config: self.library_config,
                unflushed_integrations: Vec::new(),
                unflushed_dependencies,
                unflushed_logs: HashMap::new(),
                metric_contexts: contexts.clone(),
                metric_buckets: MetricBuckets::default(),
                host: self.host,
                app: self.application,
            },
            config,
            mailbox,
            seq_id: 0,
            runtime_id: self
                .runtime_id
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            client,
            deadlines: Scheduler::new(),
            cancellation_token: token.clone(),
        };

        Ok((
            TelemetryWorkerHandle {
                sender: tx,
                shutdown,
                cancellation_token: token,
                runtime: tokio_runtime,
                contexts,
            },
            worker,
        ))
    }

    pub async fn spawn(self) -> Result<(TelemetryWorkerHandle, JoinHandle<()>)> {
        let config = config::FromEnv::config();
        self.spawn_with_config(config).await
    }

    pub async fn spawn_with_config(
        self,
        config: Config,
    ) -> Result<(TelemetryWorkerHandle, JoinHandle<()>)> {
        let tokio_runtime = tokio::runtime::Handle::current();

        let (worker_handle, worker) = self.build_worker(config, tokio_runtime.clone())?;

        let join_handle = tokio_runtime.spawn(worker.run());

        Ok((worker_handle, join_handle))
    }

    pub fn run(self) -> Result<TelemetryWorkerHandle> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let config = config::FromEnv::config();

        let (handle, worker) = self.build_worker(config, runtime.handle().clone())?;

        let notify_shutdown = handle.shutdown.clone();
        std::thread::spawn(move || {
            runtime.block_on(worker.run());
            notify_shutdown.shutdown_finished();
        });

        Ok(handle)
    }
}

/// Schedules the next action the telemetry worker is supposed to take.
/// `Scheduler::recv_next_action` either waits for the next sheduled action, or
/// returns earlier if we receive a message to process.
///
/// Action are scheduled using the  schedule_next_<action> method.
///
/// Once an action has been executed, the corresponding <action>_done should be called
/// to update the Scheduler state
struct Scheduler {
    // Triggered at fixed interval every 60s
    // If a message has been sent while this delay is pending, the  next heartbeat will be rescheduled
    heartbeat: time::Instant,
    // Triggered a few seconds after a dependecy is received to batch them together
    flush_dependencies: Option<time::Instant>,
    // Triggered a few seconds after an integrations is received to batch them together
    flush_integrations: Option<time::Instant>,
    // Triggered after 60s aggregating logs
    // to be able to deduplicate them
    flush_logs: Option<time::Instant>,
    // Triggered after some time aggregating metrics to send multiple points in a payload
    flush_metrics: Option<time::Instant>,
    // Triggered every 10s to add a point to the series we hold
    aggregate_metrics: time::Instant,
    delays: Delays,
}

// Concrete struct to be able to modify the scheduler delays for testing
struct Delays {
    heartbeat: time::Duration,
    deps_flush: time::Duration,
    intgs_flush: time::Duration,
    logs_flush: time::Duration,
    metrics_aggregation: time::Duration,
    metrics_flush: time::Duration,
}

impl Scheduler {
    const DEFAULT_DELAYS: Delays = Delays {
        heartbeat: time::Duration::from_secs(60),
        deps_flush: time::Duration::from_secs(2),
        intgs_flush: time::Duration::from_secs(2),
        logs_flush: time::Duration::from_secs(60),
        metrics_aggregation: time::Duration::from_secs(10),
        metrics_flush: time::Duration::from_secs(60),
    };

    fn new() -> Self {
        Self::new_with_delays(Self::DEFAULT_DELAYS)
    }

    fn new_with_delays(delays: Delays) -> Self {
        let now = time::Instant::now();
        Self {
            heartbeat: now + delays.heartbeat,
            flush_dependencies: None,
            flush_integrations: None,
            flush_logs: None,
            aggregate_metrics: now + delays.metrics_aggregation,
            flush_metrics: None,
            delays,
        }
    }

    fn schedule_next_heartbeat(&mut self) {
        self.heartbeat = time::Instant::now() + self.delays.heartbeat;
    }

    fn schedule_next_send_dependency(&mut self) {
        self.flush_dependencies = Some(time::Instant::now() + self.delays.deps_flush);
    }

    fn schedule_next_send_integration(&mut self) {
        self.flush_integrations = Some(time::Instant::now() + self.delays.intgs_flush);
    }

    fn schedule_next_send_logs(&mut self) {
        // Do not reschedule if a send is already scheduled to prevent stalling
        if self.flush_logs.is_none() {
            self.flush_logs = Some(time::Instant::now() + self.delays.logs_flush);
        }
    }

    fn schedule_next_send_metrics(&mut self) {
        if self.flush_metrics.is_none() {
            self.flush_metrics = Some(time::Instant::now() + self.delays.metrics_flush)
        }
    }

    fn send_dependency_done(&mut self) {
        self.flush_dependencies = None;
        self.schedule_next_heartbeat();
    }

    fn send_integrations_done(&mut self) {
        self.flush_integrations = None;
        self.schedule_next_heartbeat();
    }

    fn send_logs_done(&mut self) {
        self.flush_logs = None;
        self.schedule_next_heartbeat();
    }

    fn flush_aggreg_done(&mut self) {
        self.aggregate_metrics = time::Instant::now() + self.delays.metrics_aggregation;
    }

    fn send_metrics_done(&mut self) {
        self.flush_metrics = None;
        self.schedule_next_heartbeat();
    }

    #[inline(always)]
    fn deadlines(&self) -> impl Iterator<Item = (time::Instant, TelemetryActions)> {
        IntoIterator::into_iter([
            Some((self.heartbeat, TelemetryActions::Heartbeat)),
            Some((
                self.aggregate_metrics,
                TelemetryActions::FlushMetricAggregate,
            )),
            self.flush_dependencies
                .map(|d| (d, TelemetryActions::SendDependencies)),
            self.flush_integrations
                .map(|d| (d, TelemetryActions::SendIntegrations)),
            self.flush_logs.map(|d| (d, TelemetryActions::SendLogs)),
            self.flush_metrics
                .map(|d| (d, TelemetryActions::SendMetrics)),
        ])
        .flatten()
    }

    fn next_deadline(&self) -> Option<(time::Instant, TelemetryActions)> {
        self.deadlines().min_by_key(|(d, _)| *d)
    }

    async fn recv_next_action(
        &self,
        mailbox: &mut mpsc::Receiver<TelemetryActions>,
    ) -> TelemetryActions {
        let action = if let Some((deadline, deadline_action)) = self.next_deadline() {
            if deadline
                .checked_duration_since(time::Instant::now())
                .is_none()
            {
                return deadline_action;
            };

            match tokio::time::timeout_at(deadline.into(), mailbox.recv()).await {
                Ok(mailbox_action) => mailbox_action,
                Err(_) => Some(deadline_action),
            }
        } else {
            mailbox.recv().await
        };

        // when no action is received, then it means the channel is stopped
        action.unwrap_or(TelemetryActions::Stop)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::mem::discriminant;
    use std::time::{Duration, Instant};

    const TEST_DELAYS: Delays = Delays {
        metrics_aggregation: Duration::from_secs(999),
        ..Scheduler::DEFAULT_DELAYS
    };

    fn expect_scheduled(
        scheduler: &Scheduler,
        expected_action: TelemetryActions,
        expected_scheduled_after: Duration,
    ) {
        let next_deadline = scheduler.next_deadline().unwrap();
        let scheduled_in = next_deadline.0.duration_since(Instant::now());

        assert_eq!(
            discriminant(&next_deadline.1),
            discriminant(&expected_action)
        );
        assert!(expected_scheduled_after - Duration::from_millis(5) < scheduled_in);
        assert!(scheduled_in < expected_scheduled_after);
    }

    #[test]
    fn test_scheduler_next_heartbeat() {
        let mut scheduler = Scheduler::new_with_delays(TEST_DELAYS);

        let next_deadline = scheduler.next_deadline().unwrap();
        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );

        scheduler.schedule_next_heartbeat();

        let next_deadline2 = scheduler.next_deadline().unwrap();
        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );

        assert!(next_deadline.0 < next_deadline2.0)
    }

    #[test]
    fn test_scheduler_send_dependency() {
        let mut scheduler = Scheduler::new_with_delays(TEST_DELAYS);

        let flush_delay_ms = 222;
        scheduler.delays.deps_flush = Duration::from_millis(flush_delay_ms);

        scheduler.schedule_next_send_dependency();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendDependencies,
            scheduler.delays.deps_flush,
        );
        scheduler.send_dependency_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_send_integrations() {
        let mut scheduler = Scheduler::new_with_delays(TEST_DELAYS);

        let flush_delay_ms = 333;
        scheduler.delays.intgs_flush = Duration::from_millis(flush_delay_ms);

        scheduler.schedule_next_send_integration();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendIntegrations,
            scheduler.delays.intgs_flush,
        );

        scheduler.send_integrations_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_send_logs() {
        let mut scheduler = Scheduler::new_with_delays(TEST_DELAYS);
        scheduler.delays.logs_flush = Duration::from_millis(99);

        scheduler.schedule_next_send_logs();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendLogs,
            scheduler.delays.logs_flush,
        );

        scheduler.send_logs_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_send_metrics() {
        let mut scheduler = Scheduler::new_with_delays(TEST_DELAYS);
        scheduler.delays.metrics_flush = Duration::from_millis(99);

        scheduler.schedule_next_send_metrics();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendMetrics,
            scheduler.delays.metrics_flush,
        );

        scheduler.send_metrics_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_aggreg_metrics() {
        let mut scheduler = Scheduler::new_with_delays(Delays {
            metrics_aggregation: Duration::from_millis(88),
            ..TEST_DELAYS
        });
        expect_scheduled(
            &scheduler,
            TelemetryActions::FlushMetricAggregate,
            scheduler.delays.metrics_aggregation,
        );
        scheduler.flush_aggreg_done();
        expect_scheduled(
            &scheduler,
            TelemetryActions::FlushMetricAggregate,
            scheduler.delays.metrics_aggregation,
        );
    }
}
