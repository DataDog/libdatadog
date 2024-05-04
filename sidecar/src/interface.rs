// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::iter::{zip, Sum};
use std::ops::{Add, DerefMut, Sub};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::Result;

use futures::{
    future::{join_all, BoxFuture, Shared},
    FutureExt,
};
use lazy_static::lazy_static;
use manual_future::{ManualFuture, ManualFutureCompleter};

use datadog_ipc::platform::NamedShmHandle;

use serde::{Deserialize, Serialize};
use serde_with::{serde_as, VecSkipError};
use tokio::select;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, info, warn};

use crate::agent_remote_config::AgentRemoteConfigWriter;

use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;
use ddcommon::{tag::Tag, Endpoint};
use ddtelemetry::worker::TelemetryWorkerStats;
use ddtelemetry::{
    data,
    metrics::{ContextKey, MetricContext},
    worker::{store::Store, TelemetryActions, TelemetryWorkerHandle, MAX_ITEMS},
};

use crate::config;
use crate::log::TemporarilyRetainedMapStats;

use crate::service::{
    RuntimeMetadata, SerializedTracerHeaderTags, SidecarInterfaceRequest, SidecarInterfaceResponse,
};

#[derive(Serialize, Deserialize)]
pub struct SidecarStats {
    pub trace_flusher: TraceFlusherStats,
    pub sessions: u32,
    pub session_counter_size: u32,
    pub runtimes: u32,
    pub apps: u32,
    pub active_apps: u32,
    pub enqueued_apps: u32,
    pub enqueued_telemetry_data: EnqueuedTelemetryStats,
    pub telemetry_metrics_contexts: u32,
    pub telemetry_worker: TelemetryWorkerStats,
    pub telemetry_worker_errors: u32,
    pub log_writer: TemporarilyRetainedMapStats,
    pub log_filter: TemporarilyRetainedMapStats,
}

#[derive(Serialize, Deserialize)]
pub struct TraceFlusherStats {
    pub agent_config_allocated_shm: u32,
    pub agent_config_writers: u32,
    pub agent_configs_last_used_entries: u32,
    pub send_data_size: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum SidecarAction {
    Telemetry(TelemetryActions),
    RegisterTelemetryMetric(MetricContext),
    AddTelemetryMetricPoint((String, f64, Vec<Tag>)),
    PhpComposerTelemetryFile(PathBuf),
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum AppOrQueue {
    App(Shared<ManualFuture<(String, String)>>),
    Queue(EnqueuedTelemetryData),
}

#[derive(Clone)]
pub struct AppInstance {
    pub(crate) telemetry: TelemetryWorkerHandle,
    pub(crate) telemetry_worker_shutdown: Shared<BoxFuture<'static, Option<()>>>,
    pub(crate) telemetry_metrics: Arc<Mutex<HashMap<String, ContextKey>>>,
}

impl AppInstance {
    pub fn register_metric(&mut self, metric: MetricContext) {
        let mut metrics = self.telemetry_metrics.lock().unwrap();
        if !metrics.contains_key(&metric.name) {
            metrics.insert(
                metric.name.clone(),
                self.telemetry.register_metric_context(
                    metric.name,
                    metric.tags,
                    metric.metric_type,
                    metric.common,
                    metric.namespace,
                ),
            );
        }
    }

    pub fn to_telemetry_point(
        &self,
        (name, val, tags): (String, f64, Vec<Tag>),
    ) -> TelemetryActions {
        TelemetryActions::AddPoint((
            val,
            *self.telemetry_metrics.lock().unwrap().get(&name).unwrap(),
            tags,
        ))
    }
}

pub(crate) struct EnqueuedTelemetryData {
    dependencies: Store<data::Dependency>,
    configurations: Store<data::Configuration>,
    integrations: Store<data::Integration>,
    pub(crate) metrics: Vec<MetricContext>,
    pub(crate) points: Vec<(String, f64, Vec<Tag>)>,
    pub(crate) actions: Vec<TelemetryActions>,
    computed_dependencies: Vec<Shared<ManualFuture<Arc<Vec<data::Dependency>>>>>,
}

impl Default for EnqueuedTelemetryData {
    fn default() -> Self {
        Self {
            dependencies: Store::new(MAX_ITEMS),
            configurations: Store::new(MAX_ITEMS),
            integrations: Store::new(MAX_ITEMS),
            metrics: Vec::new(),
            points: Vec::new(),
            actions: Vec::new(),
            computed_dependencies: Vec::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct EnqueuedTelemetryStats {
    pub dependencies_stored: u32,
    pub dependencies_unflushed: u32,
    pub configurations_stored: u32,
    pub configurations_unflushed: u32,
    pub integrations_stored: u32,
    pub integrations_unflushed: u32,
    pub metrics: u32,
    pub points: u32,
    pub actions: u32,
    pub computed_dependencies: u32,
}

impl Add for EnqueuedTelemetryStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        EnqueuedTelemetryStats {
            dependencies_stored: self.dependencies_stored + rhs.dependencies_stored,
            dependencies_unflushed: self.dependencies_unflushed + rhs.dependencies_unflushed,
            configurations_stored: self.configurations_stored + rhs.configurations_stored,
            configurations_unflushed: self.configurations_unflushed + rhs.configurations_unflushed,
            integrations_stored: self.integrations_stored + rhs.integrations_stored,
            integrations_unflushed: self.integrations_unflushed + rhs.integrations_unflushed,
            metrics: self.metrics + rhs.metrics,
            points: self.points + rhs.points,
            actions: self.actions + rhs.actions,
            computed_dependencies: self.computed_dependencies + rhs.computed_dependencies,
        }
    }
}

impl Sum for EnqueuedTelemetryStats {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |a, b| a + b)
    }
}

#[serde_as]
#[derive(Deserialize)]
struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    packages: Vec<data::Dependency>,
}

impl EnqueuedTelemetryData {
    pub fn process(&mut self, actions: Vec<SidecarAction>) {
        for action in actions {
            match action {
                SidecarAction::Telemetry(TelemetryActions::AddConfig(c)) => {
                    self.configurations.insert(c)
                }
                SidecarAction::Telemetry(TelemetryActions::AddDependecy(d)) => {
                    self.dependencies.insert(d)
                }
                SidecarAction::Telemetry(TelemetryActions::AddIntegration(i)) => {
                    self.integrations.insert(i)
                }
                SidecarAction::Telemetry(other) => self.actions.push(other),
                SidecarAction::PhpComposerTelemetryFile(composer_path) => self
                    .computed_dependencies
                    .push(Self::extract_composer_telemetry(composer_path).shared()),

                SidecarAction::RegisterTelemetryMetric(m) => self.metrics.push(m),
                SidecarAction::AddTelemetryMetricPoint(p) => self.points.push(p),
            }
        }
    }

    pub fn processed(action: Vec<SidecarAction>) -> Self {
        let mut data = Self::default();
        data.process(action);
        data
    }

    pub(crate) async fn extract_telemetry_actions(&mut self, actions: &mut Vec<TelemetryActions>) {
        for computed_deps in self.computed_dependencies.clone() {
            for d in computed_deps.await.iter() {
                actions.push(TelemetryActions::AddDependecy(d.clone()));
            }
        }
        for d in self.dependencies.unflushed() {
            actions.push(TelemetryActions::AddDependecy(d.clone()));
        }
        for c in self.configurations.unflushed() {
            actions.push(TelemetryActions::AddConfig(c.clone()));
        }
        for i in self.integrations.unflushed() {
            actions.push(TelemetryActions::AddIntegration(i.clone()));
        }
    }

    pub async fn process_immediately(
        sidecar_actions: Vec<SidecarAction>,
        app: &mut AppInstance,
    ) -> Vec<TelemetryActions> {
        let mut actions = vec![];
        for action in sidecar_actions {
            match action {
                SidecarAction::Telemetry(t) => actions.push(t),
                SidecarAction::PhpComposerTelemetryFile(path) => {
                    for nested in Self::extract_composer_telemetry(path).await.iter() {
                        actions.push(TelemetryActions::AddDependecy(nested.clone()));
                    }
                }
                SidecarAction::RegisterTelemetryMetric(metric) => app.register_metric(metric),
                SidecarAction::AddTelemetryMetricPoint(point) => {
                    actions.push(app.to_telemetry_point(point));
                }
            }
        }
        actions
    }

    // This parses a vendor/composer/installed.json file. It caches the parsed result for a while.
    fn extract_composer_telemetry(path: PathBuf) -> ManualFuture<Arc<Vec<data::Dependency>>> {
        let (deps, completer) = ManualFuture::new();
        tokio::spawn(async {
            type ComposerCache = HashMap<PathBuf, (SystemTime, Arc<Vec<data::Dependency>>)>;
            lazy_static! {
                static ref COMPOSER_CACHE: tokio::sync::Mutex<ComposerCache> =
                    tokio::sync::Mutex::new(Default::default());
                static ref LAST_CACHE_CLEAN: AtomicU64 = AtomicU64::new(0);
            }

            let mut cache = COMPOSER_CACHE.lock().await;
            let packages = match tokio::fs::metadata(&path).await.and_then(|m| m.modified()) {
                Err(e) => {
                    warn!("Failed to report dependencies from {path:?}, could not read modification time: {e:?}");
                    Arc::new(vec![])
                }
                Ok(modification) => {
                    let now = SystemTime::now();
                    if let Some((last_update, actions)) = cache.get(&path) {
                        if modification < *last_update {
                            completer.complete(actions.clone()).await;
                            return;
                        }
                    }
                    async fn parse(path: &PathBuf) -> Result<Vec<data::Dependency>> {
                        let mut json = tokio::fs::read(&path).await?;
                        #[cfg(not(target_arch = "x86"))]
                        let parsed: ComposerPackages = simd_json::from_slice(json.as_mut_slice())?;
                        #[cfg(target_arch = "x86")]
                        let parsed = ComposerPackages { packages: vec![] }; // not interested in 32 bit
                        Ok(parsed.packages)
                    }
                    let packages = Arc::new(parse(&path).await.unwrap_or_else(|e| {
                        warn!("Failed to report dependencies from {path:?}: {e:?}");
                        vec![]
                    }));
                    cache.insert(path, (now, packages.clone()));
                    // cheap way to avoid unbounded caching
                    const CACHE_INTERVAL: u64 = 2000;
                    let last_clean = LAST_CACHE_CLEAN.load(Ordering::Relaxed);
                    let now_secs = Instant::now().elapsed().as_secs();
                    if now_secs > last_clean + CACHE_INTERVAL
                        && LAST_CACHE_CLEAN
                            .compare_exchange(
                                last_clean,
                                now_secs,
                                Ordering::SeqCst,
                                Ordering::Acquire,
                            )
                            .is_ok()
                    {
                        cache.retain(|_, (inserted, _)| {
                            *inserted > now.sub(Duration::from_secs(CACHE_INTERVAL))
                        });
                    }
                    packages
                }
            };
            completer.complete(packages).await;
        });
        deps
    }

    pub fn stats(&self) -> EnqueuedTelemetryStats {
        EnqueuedTelemetryStats {
            dependencies_stored: self.dependencies.len_stored() as u32,
            dependencies_unflushed: self.dependencies.len_unflushed() as u32,
            configurations_stored: self.configurations.len_stored() as u32,
            configurations_unflushed: self.configurations.len_unflushed() as u32,
            integrations_stored: self.integrations.len_stored() as u32,
            integrations_unflushed: self.integrations.len_unflushed() as u32,
            metrics: self.metrics.len() as u32,
            points: self.points.len() as u32,
            actions: self.actions.len() as u32,
            computed_dependencies: self.computed_dependencies.len() as u32,
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_extract_composer_telemetry() {
    let data = EnqueuedTelemetryData::extract_composer_telemetry(
        concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/installed.json").into(),
    )
    .await;
    assert_eq!(
        data,
        vec![
            data::Dependency {
                name: "g1a/composer-test-scenarios".to_string(),
                version: None
            },
            data::Dependency {
                name: "datadog/dd-trace".to_string(),
                version: Some("dev-master".to_string())
            },
        ]
        .into()
    );
}

#[derive(Default)]
struct TraceSendData {
    pub send_data: Vec<SendData>,
    pub send_data_size: usize,
    pub force_flush: Option<ManualFutureCompleter<()>>,
}

impl TraceSendData {
    pub fn flush(&mut self) {
        if let Some(force_flush) = self.force_flush.take() {
            debug!(
                "Emitted flush for traces with {} bytes in send_data buffer",
                self.send_data_size
            );
            tokio::spawn(async move {
                force_flush.complete(()).await;
            });
        }
    }
}

#[derive(Default)]
struct TraceFlusherData {
    pub traces: TraceSendData,
    pub flusher: Option<JoinHandle<()>>,
}

struct AgentRemoteConfig {
    pub writer: AgentRemoteConfigWriter<NamedShmHandle>,
    pub last_write: Instant,
}

#[derive(Default)]
struct AgentRemoteConfigs {
    pub writers: HashMap<Endpoint, AgentRemoteConfig>,
    pub last_used: BTreeMap<Instant, Endpoint>,
}

#[derive(Default)]
pub struct TraceFlusher {
    inner: Mutex<TraceFlusherData>,
    pub interval: AtomicU64,
    pub min_force_flush_size: AtomicU32,
    pub min_force_drop_size: AtomicU32, // put a limit on memory usage
    remote_config: Mutex<AgentRemoteConfigs>,
}

impl TraceFlusher {
    fn write_remote_configs(&self, endpoint: Endpoint, contents: Vec<u8>) {
        let configs = &mut *self.remote_config.lock().unwrap();

        let mut entry = configs.writers.entry(endpoint.clone());
        let writer = match entry {
            Entry::Occupied(ref mut entry) => entry.get_mut(),
            Entry::Vacant(entry) => {
                if let Ok(writer) = crate::agent_remote_config::new_writer(&endpoint) {
                    entry.insert(AgentRemoteConfig {
                        writer,
                        last_write: Instant::now(),
                    })
                } else {
                    return;
                }
            }
        };
        writer.writer.write(contents.as_slice());

        let now = Instant::now();
        let last = writer.last_write;
        writer.last_write = now;

        configs.last_used.remove(&last);
        configs.last_used.insert(now, endpoint);

        while let Some((&time, _)) = configs.last_used.iter().next() {
            if time + Duration::new(50, 0) > Instant::now() {
                break;
            }
            configs
                .writers
                .remove(&configs.last_used.remove(&time).unwrap());
        }
    }

    fn start_trace_flusher(self: Arc<Self>, mut force_flush: ManualFuture<()>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                select! {
                    _ = tokio::time::sleep(Duration::from_millis(
                        self.interval.load(Ordering::Relaxed),
                    )) => {},
                    _ = force_flush => {},
                }

                debug!(
                    "Start flushing {} bytes worth of traces",
                    self.inner.lock().unwrap().traces.send_data_size
                );

                let (new_force_flush, completer) = ManualFuture::new();
                force_flush = new_force_flush;

                let trace_buffer = std::mem::replace(
                    &mut self.inner.lock().unwrap().traces,
                    TraceSendData {
                        send_data: vec![],
                        send_data_size: 0,
                        force_flush: Some(completer),
                    },
                )
                .send_data;
                let mut futures: Vec<_> = Vec::new();
                let mut intake_target: Vec<_> = Vec::new();
                for send_data in trace_utils::coalesce_send_data(trace_buffer).into_iter() {
                    intake_target.push(send_data.target.clone());
                    futures.push(send_data.send());
                }
                for (endpoint, response) in zip(intake_target, join_all(futures).await) {
                    match response {
                        Ok(response) => {
                            if endpoint.api_key.is_none() {
                                // not when intake
                                match hyper::body::to_bytes(response.into_body()).await {
                                    Ok(body_bytes) => self.write_remote_configs(
                                        endpoint.clone(),
                                        body_bytes.to_vec(),
                                    ),
                                    Err(e) => error!("Error receiving agent configuration: {e:?}"),
                                }
                            }
                            info!("Successfully flushed traces to {}", endpoint.url);
                        }
                        Err(e) => {
                            error!("Error sending trace: {e:?}");
                            if endpoint.api_key.is_some() {
                                // TODO: Retries when sending to intake
                            }
                        }
                    }
                }

                let mut data = self.inner.lock().unwrap();
                let data = data.deref_mut();
                if data.traces.send_data.is_empty() {
                    data.flusher = None;
                    break;
                }
            }
        })
    }

    pub fn enqueue(self: &Arc<Self>, data: SendData) {
        let mut flush_data = self.inner.lock().unwrap();
        let flush_data = flush_data.deref_mut();

        flush_data.traces.send_data_size += data.size();

        if flush_data.traces.send_data_size
            > self.min_force_drop_size.load(Ordering::Relaxed) as usize
        {
            return;
        }

        flush_data.traces.send_data.push(data);
        if flush_data.flusher.is_none() {
            let (force_flush, completer) = ManualFuture::new();
            flush_data.flusher = Some(self.clone().start_trace_flusher(force_flush));
            flush_data.traces.force_flush = Some(completer);
        }
        if flush_data.traces.send_data_size
            > self.min_force_flush_size.load(Ordering::Relaxed) as usize
        {
            flush_data.traces.flush();
        }
    }

    pub async fn join(&self) -> Result<(), JoinError> {
        let flusher = {
            let mut flush_data = self.inner.lock().unwrap();
            self.interval.store(0, Ordering::SeqCst);
            flush_data.traces.flush();
            flush_data.deref_mut().flusher.take()
        };
        if let Some(flusher) = flusher {
            flusher.await
        } else {
            Ok(())
        }
    }

    pub fn stats(&self) -> TraceFlusherStats {
        let rc = self.remote_config.lock().unwrap();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self.inner.lock().unwrap().traces.send_data_size as u32,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionConfig {
    pub endpoint: Endpoint,
    pub flush_interval: Duration,
    pub force_flush_size: usize,
    pub force_drop_size: usize,
    pub log_level: String,
    pub log_file: config::LogMethod,
}

pub mod blocking {
    use datadog_ipc::platform::ShmHandle;
    use std::{
        borrow::Cow,
        io,
        time::{Duration, Instant},
    };

    use datadog_ipc::transport::blocking::BlockingTransport;

    use crate::interface::{SerializedTracerHeaderTags, SessionConfig, SidecarAction};
    use crate::service::{InstanceId, QueueId};

    use super::{RuntimeMetadata, SidecarInterfaceRequest, SidecarInterfaceResponse};

    pub type SidecarTransport =
        BlockingTransport<SidecarInterfaceResponse, SidecarInterfaceRequest>;

    pub fn shutdown_runtime(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::ShutdownRuntime {
            instance_id: instance_id.clone(),
        })
    }

    pub fn shutdown_session(
        transport: &mut SidecarTransport,
        session_id: String,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::ShutdownSession { session_id })
    }

    pub fn enqueue_actions(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        actions: Vec<SidecarAction>,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::EnqueueActions {
            instance_id: instance_id.clone(),
            queue_id: *queue_id,
            actions,
        })
    }

    pub fn register_service_and_flush_queued_actions(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        runtime_metadata: &RuntimeMetadata,
        service_name: Cow<str>,
        env_name: Cow<str>,
    ) -> io::Result<()> {
        transport.send(
            SidecarInterfaceRequest::RegisterServiceAndFlushQueuedActions {
                instance_id: instance_id.clone(),
                queue_id: *queue_id,
                meta: runtime_metadata.clone(),
                service_name: service_name.into_owned(),
                env_name: env_name.into_owned(),
            },
        )
    }

    pub fn set_session_config(
        transport: &mut SidecarTransport,
        session_id: String,
        config: &SessionConfig,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SetSessionConfig {
            session_id,
            config: config.clone(),
        })
    }

    pub fn send_trace_v04_bytes(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SendTraceV04Bytes {
            instance_id: instance_id.clone(),
            data,
            headers,
        })
    }

    pub fn send_trace_v04_shm(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        handle: ShmHandle,
        headers: SerializedTracerHeaderTags,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SendTraceV04Shm {
            instance_id: instance_id.clone(),
            handle,
            headers,
        })
    }

    pub fn dump(transport: &mut SidecarTransport) -> io::Result<String> {
        let res = transport.call(SidecarInterfaceRequest::Dump {})?;
        if let SidecarInterfaceResponse::Dump(dump) = res {
            Ok(dump)
        } else {
            Ok("".to_string())
        }
    }

    pub fn stats(transport: &mut SidecarTransport) -> io::Result<String> {
        let res = transport.call(SidecarInterfaceRequest::Stats {})?;
        if let SidecarInterfaceResponse::Stats(stats) = res {
            Ok(stats)
        } else {
            Ok("".to_string())
        }
    }

    pub fn ping(transport: &mut SidecarTransport) -> io::Result<Duration> {
        let start = Instant::now();
        transport.call(SidecarInterfaceRequest::Ping {})?;

        Ok(Instant::now()
            .checked_duration_since(start)
            .unwrap_or_default())
    }
}
