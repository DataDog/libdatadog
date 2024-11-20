// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod builder;
pub mod http_client;
mod scheduler;
pub mod store;

use crate::{
    config::{self, Config},
    data::{self, Application, Dependency, Host, Integration, Log, Payload, Telemetry},
    metrics::{ContextKey, MetricBuckets, MetricContexts},
    worker::builder::ConfigBuilder,
};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;

use std::iter::Sum;
use std::ops::Add;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::ControlFlow,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time,
};

use crate::metrics::MetricBucketStats;
use anyhow::Result;
use futures::{
    channel::oneshot,
    future::{self},
};
use http::{header, HeaderValue, Request};
use serde::{Deserialize, Serialize};
use tokio::{
    runtime::{self, Handle},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const CONTINUE: ControlFlow<()> = ControlFlow::Continue(());
const BREAK: ControlFlow<()> = ControlFlow::Break(());

fn time_now() -> f64 {
    std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .unwrap_or_default()
        .as_secs_f64()
}

macro_rules! telemetry_worker_log {
    ($worker:expr , ERROR , $fmt_str:tt, $($arg:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            tracing::error!($fmt_str, $($arg)*);
            if $worker.config.telemetry_debug_logging_enabled {
                eprintln!(concat!("{}: Telemetry worker ERROR: ", $fmt_str), time_now(), $($arg)*);
            }
        }
    };
    ($worker:expr , DEBUG , $fmt_str:tt, $($arg:tt)*) => {
        #[cfg(feature = "tracing")]
        tracing::debug!($fmt_str, $($arg)*);
        if $worker.config.telemetry_debug_logging_enabled {
            println!(concat!("{}: Telemetry worker DEBUG: ", $fmt_str), time_now(), $($arg)*);
        }
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub enum TelemetryActions {
    AddPoint((f64, ContextKey, Vec<Tag>)),
    AddConfig(data::Configuration),
    AddDependecy(Dependency),
    AddIntegration(Integration),
    AddLog((LogIdentifier, Log)),
    Lifecycle(LifecycleAction),
    #[serde(skip)]
    CollectStats(oneshot::Sender<TelemetryWorkerStats>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleAction {
    Start,
    Stop,
    FlushMetricAggr,
    FlushData,
    ExtendedHeartbeat,
}

/// Identifies a logging location uniquely
///
/// The identifier is a single 64 bit integer to save space an memory
/// and to be able to generic on the way different languages handle
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogIdentifier {
    // Collisions? Never heard of them
    indentifier: u64,
}

// Holds the current state of the telemetry worker
struct TelemetryWorkerData {
    started: bool,
    dependencies: store::Store<Dependency>,
    configurations: store::Store<data::Configuration>,
    integrations: store::Store<data::Integration>,
    logs: store::QueueHashMap<LogIdentifier, Log>,
    metric_contexts: MetricContexts,
    metric_buckets: MetricBuckets,
    host: Host,
    app: Application,
}

pub struct TelemetryWorker {
    config: Config,
    mailbox: mpsc::Receiver<TelemetryActions>,
    cancellation_token: CancellationToken,
    seq_id: AtomicU64,
    runtime_id: String,
    client: Box<dyn http_client::HttpClient + Sync + Send>,
    deadlines: scheduler::Scheduler<LifecycleAction>,
    data: TelemetryWorkerData,
}

#[derive(Default, Serialize, Deserialize)]
pub struct TelemetryWorkerStats {
    pub dependencies_stored: u32,
    pub dependencies_unflushed: u32,
    pub configurations_stored: u32,
    pub configurations_unflushed: u32,
    pub integrations_stored: u32,
    pub integrations_unflushed: u32,
    pub logs: u32,
    pub metric_contexts: u32,
    pub metric_buckets: MetricBucketStats,
}

impl Add for TelemetryWorkerStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        TelemetryWorkerStats {
            dependencies_stored: self.dependencies_stored + rhs.dependencies_stored,
            dependencies_unflushed: self.dependencies_unflushed + rhs.dependencies_unflushed,
            configurations_stored: self.configurations_stored + rhs.configurations_stored,
            configurations_unflushed: self.configurations_unflushed + rhs.configurations_unflushed,
            integrations_stored: self.integrations_stored + rhs.integrations_stored,
            integrations_unflushed: self.integrations_unflushed + rhs.integrations_unflushed,
            logs: self.logs + rhs.logs,
            metric_contexts: self.metric_contexts + rhs.metric_contexts,
            metric_buckets: MetricBucketStats {
                buckets: self.metric_buckets.buckets + rhs.metric_buckets.buckets,
                series: self.metric_buckets.series + rhs.metric_buckets.series,
                series_points: self.metric_buckets.series_points + rhs.metric_buckets.series_points,
                distributions: self.metric_buckets.distributions
                    + self.metric_buckets.distributions,
                distributions_points: self.metric_buckets.distributions_points
                    + self.metric_buckets.distributions_points,
            },
        }
    }
}

impl Sum for TelemetryWorkerStats {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |a, b| a + b)
    }
}

mod serialize {
    use crate::data;
    use http::HeaderValue;
    #[allow(clippy::declare_interior_mutable_const)]
    pub const CONTENT_TYPE_VALUE: HeaderValue = ddcommon::header::APPLICATION_JSON;
    pub fn serialize(telemetry: &data::Telemetry) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(telemetry)?)
    }
}

impl TelemetryWorker {
    fn log_err(&self, err: &anyhow::Error) {
        telemetry_worker_log!(self, ERROR, "{}", err);
    }

    async fn recv_next_action(&mut self) -> TelemetryActions {
        let action = if let Some((deadline, deadline_action)) = self.deadlines.next_deadline() {
            // If deadline passed, directly return associated action
            if deadline
                .checked_duration_since(time::Instant::now())
                .is_none()
            {
                return TelemetryActions::Lifecycle(*deadline_action);
            };

            // Otherwise run it in a timeout against the mailbox
            match tokio::time::timeout_at(deadline.into(), self.mailbox.recv()).await {
                Ok(mailbox_action) => mailbox_action,
                Err(_) => Some(TelemetryActions::Lifecycle(*deadline_action)),
            }
        } else {
            self.mailbox.recv().await
        };

        // if no action is received, then it means the channel is stopped
        action.unwrap_or_else(|| {
            // the worker handle no longer lives - we must remove restartable here to avoid leaks
            self.config.restartable = false;
            TelemetryActions::Lifecycle(LifecycleAction::Stop)
        })
    }

    async fn run_metrics_logs(mut self) {
        loop {
            if self.cancellation_token.is_cancelled() {
                return;
            }

            let action = self.recv_next_action().await;

            match self.dispatch_metrics_logs_action(action).await {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    if !self.config.restartable {
                        break;
                    }
                }
            };
        }
    }

    async fn dispatch_metrics_logs_action(&mut self, action: TelemetryActions) -> ControlFlow<()> {
        telemetry_worker_log!(self, DEBUG, "Handling metric action {:?}", action);
        use LifecycleAction::*;
        use TelemetryActions::*;
        match action {
            Lifecycle(Start) => {
                if !self.data.started {
                    self.deadlines
                        .schedule_event(LifecycleAction::FlushData)
                        .unwrap();
                    self.deadlines
                        .schedule_event(LifecycleAction::FlushMetricAggr)
                        .unwrap();
                    self.data.started = true;
                }
            }
            AddLog((identifier, log)) => {
                let (l, new) = self.data.logs.get_mut_or_insert(identifier, log);
                if !new {
                    l.count += 1;
                }
            }
            AddPoint((point, key, extra_tags)) => {
                self.data.metric_buckets.add_point(key, point, extra_tags)
            }
            Lifecycle(FlushMetricAggr) => {
                self.data.metric_buckets.flush_agregates();
                self.deadlines
                    .schedule_event(LifecycleAction::FlushMetricAggr)
                    .unwrap();
            }
            Lifecycle(FlushData) => {
                if !self.data.started {
                    return CONTINUE;
                }
                let batch = self.build_observability_batch();
                if !batch.is_empty() {
                    let payload = data::Payload::MessageBatch(batch);
                    match self.send_payload(&payload).await {
                        Ok(()) => self.payload_sent_success(&payload),
                        Err(e) => self.log_err(&e),
                    }
                }

                self.deadlines
                    .schedule_event(LifecycleAction::FlushData)
                    .unwrap();
            }
            AddConfig(_) | AddDependecy(_) | AddIntegration(_) | Lifecycle(ExtendedHeartbeat) => {}
            Lifecycle(Stop) => {
                if !self.data.started {
                    return BREAK;
                }
                self.data.metric_buckets.flush_agregates();

                let obsevability_events = self.build_observability_batch();
                if let Err(e) = self
                    .send_payload(&data::Payload::MessageBatch(obsevability_events))
                    .await
                {
                    self.log_err(&e);
                }
                self.data.started = false;
                self.deadlines.clear_pending();
                return BREAK;
            }
            CollectStats(stats_sender) => {
                stats_sender.send(self.stats()).ok();
            }
        };
        CONTINUE
    }

    // Runs a state machine that waits for actions, either from the worker's
    // mailbox, or scheduled actions from the worker's deadline object.
    async fn run(mut self) {
        loop {
            if self.cancellation_token.is_cancelled() {
                return;
            }

            let action = self.recv_next_action().await;

            match self.dispatch_action(action).await {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    if !self.config.restartable {
                        break;
                    }
                }
            };
        }
    }

    async fn dispatch_action(&mut self, action: TelemetryActions) -> ControlFlow<()> {
        telemetry_worker_log!(self, DEBUG, "Handling action {:?}", action);

        use LifecycleAction::*;
        use TelemetryActions::*;
        match action {
            Lifecycle(Start) => {
                if !self.data.started {
                    let app_started = data::Payload::AppStarted(self.build_app_started());
                    match self.send_payload(&app_started).await {
                        Ok(()) => self.payload_sent_success(&app_started),
                        Err(err) => self.log_err(&err),
                    }
                    self.deadlines
                        .schedule_event(LifecycleAction::FlushData)
                        .unwrap();
                    self.deadlines
                        .schedule_event(LifecycleAction::FlushMetricAggr)
                        .unwrap();
                    self.data.started = true;
                }
            }
            AddDependecy(dep) => self.data.dependencies.insert(dep),
            AddIntegration(integration) => self.data.integrations.insert(integration),
            AddConfig(cfg) => self.data.configurations.insert(cfg),
            AddLog((identifier, log)) => {
                let (l, new) = self.data.logs.get_mut_or_insert(identifier, log);
                if !new {
                    l.count += 1;
                }
            }
            AddPoint((point, key, extra_tags)) => {
                self.data.metric_buckets.add_point(key, point, extra_tags)
            }
            Lifecycle(FlushMetricAggr) => {
                self.data.metric_buckets.flush_agregates();
                self.deadlines
                    .schedule_event(LifecycleAction::FlushMetricAggr)
                    .unwrap();
            }
            Lifecycle(FlushData) => {
                if !self.data.started {
                    return CONTINUE;
                }
                let mut batch = self.build_app_events_batch();
                let payload = if batch.is_empty() {
                    data::Payload::AppHeartbeat(())
                } else {
                    batch.push(data::Payload::AppHeartbeat(()));
                    data::Payload::MessageBatch(batch)
                };
                match self.send_payload(&payload).await {
                    Ok(()) => self.payload_sent_success(&payload),
                    Err(err) => self.log_err(&err),
                }

                let batch = self.build_observability_batch();
                if !batch.is_empty() {
                    let payload = data::Payload::MessageBatch(batch);
                    match self.send_payload(&payload).await {
                        Ok(()) => self.payload_sent_success(&payload),
                        Err(err) => self.log_err(&err),
                    }
                }

                self.deadlines
                    .schedule_event(LifecycleAction::FlushData)
                    .unwrap();
            }
            Lifecycle(ExtendedHeartbeat) => {
                self.data.dependencies.unflush_stored();
                self.data.integrations.unflush_stored();
                self.data.configurations.unflush_stored();

                let app_started = data::Payload::AppStarted(self.build_app_started());
                match self.send_payload(&app_started).await {
                    Ok(()) => self.payload_sent_success(&app_started),
                    Err(err) => self.log_err(&err),
                }
                self.deadlines
                    .schedule_events(
                        &mut [
                            LifecycleAction::FlushData,
                            LifecycleAction::ExtendedHeartbeat,
                        ]
                        .into_iter(),
                    )
                    .unwrap();
            }
            Lifecycle(Stop) => {
                if !self.data.started {
                    return BREAK;
                }
                self.data.metric_buckets.flush_agregates();

                let mut app_events = self.build_app_events_batch();
                app_events.push(data::Payload::AppClosing(()));

                let obsevability_events = self.build_observability_batch();

                future::join_all(
                    [
                        Some(self.build_request(&data::Payload::MessageBatch(app_events))),
                        if obsevability_events.is_empty() {
                            None
                        } else {
                            Some(
                                self.build_request(&data::Payload::MessageBatch(
                                    obsevability_events,
                                )),
                            )
                        },
                    ]
                    .into_iter()
                    .flatten()
                    .filter_map(|r| match r {
                        Ok(r) => Some(r),
                        Err(e) => {
                            self.log_err(&e);
                            None
                        }
                    })
                    .map(|r| async {
                        if let Err(e) = self.send_request(r).await {
                            self.log_err(&e);
                        }
                    }),
                )
                .await;

                self.data.started = false;
                self.deadlines.clear_pending();
                return BREAK;
            }
            CollectStats(stats_sender) => {
                stats_sender.send(self.stats()).ok();
            }
        }

        CONTINUE
    }

    // Builds telemetry payloads containing lifecycle events
    fn build_app_events_batch(&self) -> Vec<Payload> {
        let mut payloads = Vec::new();

        if self.data.dependencies.flush_not_empty() {
            payloads.push(data::Payload::AppDependenciesLoaded(
                data::AppDependenciesLoaded {
                    dependencies: self.data.dependencies.unflushed().cloned().collect(),
                },
            ))
        }
        if self.data.integrations.flush_not_empty() {
            payloads.push(data::Payload::AppIntegrationsChange(
                data::AppIntegrationsChange {
                    integrations: self.data.integrations.unflushed().cloned().collect(),
                },
            ))
        }
        if self.data.configurations.flush_not_empty() {
            payloads.push(data::Payload::AppClientConfigurationChange(
                data::AppClientConfigurationChange {
                    configuration: self.data.configurations.unflushed().cloned().collect(),
                },
            ))
        }
        payloads
    }

    // Builds telemetry payloads containing logs, metrics and distributions
    fn build_observability_batch(&mut self) -> Vec<Payload> {
        let mut payloads = Vec::new();

        let logs = self.build_logs();
        if !logs.is_empty() {
            payloads.push(data::Payload::Logs(logs));
        }
        let metrics = self.build_metrics_series();
        if !metrics.series.is_empty() {
            payloads.push(data::Payload::GenerateMetrics(metrics))
        }
        let distributions = self.build_metrics_distributions();
        if !distributions.series.is_empty() {
            payloads.push(data::Payload::Sketches(distributions))
        }
        payloads
    }

    fn build_metrics_distributions(&mut self) -> data::Distributions {
        let mut series = Vec::new();
        let context_guard = self.data.metric_contexts.lock();
        for (context_key, extra_tags, points) in self.data.metric_buckets.flush_distributions() {
            let Some(context) = context_guard.read(context_key) else {
                telemetry_worker_log!(self, ERROR, "Context not found for key {:?}", context_key);
                continue;
            };
            let mut tags = extra_tags;
            tags.extend(context.tags.iter().cloned());
            series.push(data::metrics::Distribution {
                namespace: context.namespace,
                metric: context.name.clone(),
                tags,
                sketch: data::metrics::SerializedSketch::B64 {
                    sketch_b64: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        points.encode_to_vec(),
                    ),
                },
                common: context.common,
                _type: context.metric_type,
                interval: MetricBuckets::METRICS_FLUSH_INTERVAL.as_secs(),
            });
        }
        data::Distributions { series }
    }

    fn build_metrics_series(&mut self) -> data::GenerateMetrics {
        let mut series = Vec::new();
        let context_guard = self.data.metric_contexts.lock();
        for (context_key, extra_tags, points) in self.data.metric_buckets.flush_series() {
            let Some(context) = context_guard.read(context_key) else {
                telemetry_worker_log!(self, ERROR, "Context not found for key {:?}", context_key);
                continue;
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
                interval: MetricBuckets::METRICS_FLUSH_INTERVAL.as_secs(),
            });
        }

        data::GenerateMetrics { series }
    }

    fn build_app_started(&mut self) -> data::AppStarted {
        data::AppStarted {
            configuration: self.data.configurations.unflushed().cloned().collect(),
        }
    }

    fn app_started_sent_success(&mut self, p: &data::AppStarted) {
        self.data
            .configurations
            .removed_flushed(p.configuration.len());
    }

    fn payload_sent_success(&mut self, payload: &data::Payload) {
        use data::Payload::*;
        match payload {
            AppStarted(p) => self.app_started_sent_success(p),
            AppExtendedHeartbeat(p) => self.app_started_sent_success(p),
            AppDependenciesLoaded(p) => {
                self.data.dependencies.removed_flushed(p.dependencies.len())
            }
            AppIntegrationsChange(p) => {
                self.data.integrations.removed_flushed(p.integrations.len())
            }
            AppClientConfigurationChange(p) => self
                .data
                .configurations
                .removed_flushed(p.configuration.len()),
            MessageBatch(batch) => {
                for p in batch {
                    self.payload_sent_success(p);
                }
            }
            Logs(p) => {
                for _ in p {
                    self.data.logs.pop_front();
                }
            }
            AppHeartbeat(()) | AppClosing(()) => {}
            GenerateMetrics(_) | Sketches(_) => {}
        }
    }

    fn build_logs(&self) -> Vec<Log> {
        // TODO: change the data model to take a &[Log] so don't have to clone data here
        let logs = self.data.logs.iter().map(|(_, l)| l.clone()).collect();
        logs
    }

    fn next_seq_id(&self) -> u64 {
        self.seq_id.fetch_add(1, Ordering::Release)
    }

    async fn send_payload(&self, payload: &data::Payload) -> Result<()> {
        let req = self.build_request(payload)?;
        self.send_request(req).await
    }

    fn build_request(&self, payload: &data::Payload) -> Result<Request<hyper::Body>> {
        let seq_id = self.next_seq_id();
        let tel = Telemetry {
            api_version: data::ApiVersion::V2,
            tracer_time: time::SystemTime::UNIX_EPOCH
                .elapsed()
                .map_or(0, |d| d.as_secs()),
            runtime_id: &self.runtime_id,
            seq_id,
            host: &self.data.host,
            application: &self.data.app,
            payload,
        };

        telemetry_worker_log!(self, DEBUG, "Prepared payload: {:?}", tel);

        let req = http_client::request_builder(&self.config)?
            .method(http::Method::POST)
            .header(header::CONTENT_TYPE, serialize::CONTENT_TYPE_VALUE)
            .header(
                http_client::header::REQUEST_TYPE,
                HeaderValue::from_static(payload.request_type()),
            )
            .header(
                http_client::header::API_VERSION,
                HeaderValue::from_static(data::ApiVersion::V2.to_str()),
            )
            .header(
                http_client::header::LIBRARY_LANGUAGE,
                // Note: passing by ref here just causes the clone to happen underneath
                tel.application.language_name.clone(),
            )
            .header(
                http_client::header::LIBRARY_VERSION,
                &tel.application.tracer_version.clone(),
            );

        let body = hyper::Body::from(serialize::serialize(&tel)?);
        Ok(req.body(body)?)
    }

    async fn send_request(&self, req: Request<hyper::Body>) -> Result<()> {
        tokio::select! {
            _ = self.cancellation_token.cancelled() => {
                Err(anyhow::anyhow!("Request cancelled"))
            },
            _ = tokio::time::sleep(time::Duration::from_millis(
                    if let Some(endpoint) = self.config.endpoint.as_ref() {
                        endpoint.timeout_ms
                    } else {
                        Endpoint::DEFAULT_TIMEOUT
                    })) => {
                Err(anyhow::anyhow!("Request timed out"))
            },
            r = self.client.request(req) => {
                match r {
                    Ok(_) => {
                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    fn stats(&self) -> TelemetryWorkerStats {
        TelemetryWorkerStats {
            dependencies_stored: self.data.dependencies.len_stored() as u32,
            dependencies_unflushed: self.data.dependencies.len_unflushed() as u32,
            configurations_stored: self.data.configurations.len_stored() as u32,
            configurations_unflushed: self.data.configurations.len_unflushed() as u32,
            integrations_stored: self.data.integrations.len_stored() as u32,
            integrations_unflushed: self.data.integrations.len_unflushed() as u32,
            logs: self.data.logs.len() as u32,
            metric_contexts: self.data.metric_contexts.lock().len() as u32,
            metric_buckets: self.data.metric_buckets.stats(),
        }
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
/// TelemetryWorkerHandle is a handle which allows interactions with the telemetry worker.
/// The handle is safe to use across threads.
///
/// The worker won't send data to the agent until you call `TelemetryWorkerHandle::send_start`
///
/// To stop the worker, call `TelemetryWorkerHandle::send_stop` which trigger flush asynchronously
/// then `TelemetryWorkerHandle::wait_for_shutdown`
pub struct TelemetryWorkerHandle {
    sender: mpsc::Sender<TelemetryActions>,
    shutdown: Arc<InnerTelemetryShutdown>,
    cancellation_token: CancellationToken,
    // Used to spawn cancellation tasks
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
        Ok(self
            .sender
            .try_send(TelemetryActions::Lifecycle(LifecycleAction::Start))?)
    }

    pub fn send_stop(&self) -> Result<()> {
        Ok(self
            .sender
            .try_send(TelemetryActions::Lifecycle(LifecycleAction::Stop))?)
    }

    fn cancel_requests_with_deadline(&self, deadline: time::Instant) {
        let token = self.cancellation_token.clone();
        let f = async move {
            tokio::time::sleep_until(deadline.into()).await;
            token.cancel()
        };
        self.runtime.spawn(f);
    }

    pub fn wait_for_shutdown_deadline(&self, deadline: time::Instant) {
        self.cancel_requests_with_deadline(deadline);
        self.wait_for_shutdown()
    }

    pub fn add_dependency(&self, name: String, version: Option<String>) -> Result<()> {
        self.sender
            .try_send(TelemetryActions::AddDependecy(Dependency { name, version }))?;
        Ok(())
    }

    pub fn add_integration(
        &self,
        name: String,
        enabled: bool,
        version: Option<String>,
        compatible: Option<bool>,
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
                count: 1,
                tags: String::new(),
                is_sensitive: false,
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

    pub fn stats(&self) -> Result<oneshot::Receiver<TelemetryWorkerStats>> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .try_send(TelemetryActions::CollectStats(sender))?;
        Ok(receiver)
    }
}

/// How many dependencies/integrations/configs we keep in memory at most
pub const MAX_ITEMS: usize = 5000;

pub struct TelemetryWorkerBuilder {
    pub host: Host,
    pub application: Application,
    pub runtime_id: Option<String>,
    pub dependencies: store::Store<data::Dependency>,
    pub integrations: store::Store<data::Integration>,
    pub configurations: store::Store<data::Configuration>,
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
            ..Self::new(
                String::new(),
                service_name,
                language_name,
                language_version,
                tracer_version,
            )
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
            dependencies: store::Store::new(MAX_ITEMS),
            integrations: store::Store::new(MAX_ITEMS),
            configurations: store::Store::new(MAX_ITEMS),
            native_deps: true,
            rust_shared_lib_deps: false,
            config: ConfigBuilder::default(),
        }
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
        let config = self.config.merge(external_config);
        let telemetry_hearbeat_interval = config.telemetry_hearbeat_interval;
        let client = http_client::from_config(&config);

        let worker = TelemetryWorker {
            data: TelemetryWorkerData {
                started: false,
                dependencies: self.dependencies,
                integrations: self.integrations,
                configurations: self.configurations,
                logs: store::QueueHashMap::default(),
                metric_contexts: contexts.clone(),
                metric_buckets: MetricBuckets::default(),
                host: self.host,
                app: self.application,
            },
            config,
            mailbox,
            seq_id: AtomicU64::new(1),
            runtime_id: self
                .runtime_id
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            client,
            deadlines: scheduler::Scheduler::new(vec![
                (
                    MetricBuckets::METRICS_FLUSH_INTERVAL,
                    LifecycleAction::FlushMetricAggr,
                ),
                (telemetry_hearbeat_interval, LifecycleAction::FlushData),
                (
                    time::Duration::from_secs(60 * 60 * 24),
                    LifecycleAction::ExtendedHeartbeat,
                ),
            ]),
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
        // TODO Paul LGDC: Is that really what we want?
        let config = config::Config::from_env();
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

    // Starts a telemetry worker that only sends metrics and logs, no lifecycle events
    pub fn run_metrics_logs(self) -> Result<TelemetryWorkerHandle> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let config = config::Config::from_env();

        let (handle, worker) = self.build_worker(config, runtime.handle().clone())?;
        let notify_shutdown = handle.shutdown.clone();
        std::thread::spawn(move || {
            runtime.block_on(worker.run_metrics_logs());
            runtime.shutdown_background();
            notify_shutdown.shutdown_finished();
        });

        Ok(handle)
    }

    pub fn run(self) -> Result<TelemetryWorkerHandle> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let config = config::Config::from_env();
        let (handle, worker) = self.build_worker(config, runtime.handle().clone())?;

        let notify_shutdown = handle.shutdown.clone();
        std::thread::spawn(move || {
            runtime.block_on(worker.run());
            runtime.shutdown_background();
            notify_shutdown.shutdown_finished();
        });

        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use crate::worker::TelemetryWorkerHandle;

    fn is_send<T: Send>(_: T) {}
    fn is_sync<T: Sync>(_: T) {}

    #[test]
    fn test_handle_sync_send() {
        #[allow(clippy::redundant_closure)]
        let _ = |h: TelemetryWorkerHandle| is_send(h);
        #[allow(clippy::redundant_closure)]
        let _ = |h: TelemetryWorkerHandle| is_sync(h);
    }
}
