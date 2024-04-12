// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Lint removed from stable clippy after rust 1.60 - this allow can be removed once we update rust
// version
#![allow(clippy::needless_collect)]
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashSet};
use std::iter::zip;
use std::ops::{DerefMut, Sub};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time;
use std::time::{Duration, Instant, SystemTime};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Result;

use datadog_ipc::{platform::AsyncChannel, transport::Transport};
use futures::{
    future::{self, join_all, BoxFuture, Ready, Shared},
    FutureExt,
};
use lazy_static::lazy_static;
use manual_future::{ManualFuture, ManualFutureCompleter};

use datadog_ipc::platform::{FileBackedHandle, NamedShmHandle, ShmHandle};
use datadog_ipc::tarpc::{context::Context, server::Channel};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, VecSkipError};
use tokio::select;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, enabled, error, info, warn, Level};

use crate::agent_remote_config::AgentRemoteConfigWriter;
use crate::config::get_product_endpoint;
use datadog_ipc::tarpc;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::{SendData, TracerHeaderTags};
use ddcommon::{tag::Tag, Endpoint};
use ddtelemetry::{
    data,
    metrics::{ContextKey, MetricContext},
    worker::{
        store::Store, LifecycleAction, TelemetryActions, TelemetryWorkerBuilder,
        TelemetryWorkerHandle, MAX_ITEMS,
    },
};

use crate::log::{MultiEnvFilterGuard, MultiWriterGuard};
use crate::{config, log, tracer};

#[datadog_sidecar_macros::extract_request_id]
#[datadog_ipc_macros::impl_transfer_handles]
#[tarpc::service]
pub trait SidecarInterface {
    async fn enqueue_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    );
    async fn register_service_and_flush_queued_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        meta: RuntimeMeta,
        service_name: String,
        env_name: String,
    );
    async fn set_session_config(session_id: String, config: SessionConfig);
    async fn shutdown_runtime(instance_id: InstanceId);
    async fn shutdown_session(session_id: String);
    async fn send_trace_v04_shm(
        instance_id: InstanceId,
        #[SerializedHandle] handle: ShmHandle,
        headers: SerializedTracerHeaderTags,
    );
    async fn send_trace_v04_bytes(
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    );
    async fn ping();
    async fn dump() -> String;
}

pub trait RequestIdentification {
    fn extract_identifier(&self) -> RequestIdentifier;
}

pub enum RequestIdentifier {
    InstanceId(InstanceId),
    SessionId(String),
    None,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum SidecarAction {
    Telemetry(TelemetryActions),
    RegisterTelemetryMetric(MetricContext),
    AddTelemetryMetricPoint((String, f64, Vec<Tag>)),
    PhpComposerTelemetryFile(PathBuf),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializedTracerHeaderTags {
    data: Vec<u8>,
}

impl<'a> From<&'a SerializedTracerHeaderTags> for TracerHeaderTags<'a> {
    fn from(serialized: &'a SerializedTracerHeaderTags) -> Self {
        // Panics if deserialization fails (but that shouldn't ever happen)
        bincode::deserialize(serialized.data.as_slice()).unwrap()
    }
}

impl<'a> From<TracerHeaderTags<'a>> for SerializedTracerHeaderTags {
    fn from(value: TracerHeaderTags<'a>) -> Self {
        SerializedTracerHeaderTags {
            data: bincode::serialize(&value).unwrap(),
        }
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeMeta {
    language_name: String,
    language_version: String,
    tracer_version: String,
}

impl RuntimeMeta {
    pub fn new<T>(language_name: T, language_version: T, tracer_version: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            language_name: language_name.into(),
            language_version: language_version.into(),
            tracer_version: tracer_version.into(),
        }
    }
}

#[derive(Default, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstanceId {
    session_id: String,
    runtime_id: String,
}

impl InstanceId {
    pub fn new<T>(session_id: T, runtime_id: T) -> Self
    where
        T: Into<String>,
    {
        InstanceId {
            session_id: session_id.into(),
            runtime_id: runtime_id.into(),
        }
    }
}

#[derive(Default, Copy, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct QueueId {
    inner: u64,
}

impl QueueId {
    pub fn new_unique() -> Self {
        Self {
            inner: rand::thread_rng().gen_range(1u64..u64::MAX),
        }
    }
}

#[derive(Default, Clone)]
struct SessionInfo {
    runtimes: Arc<Mutex<HashMap<String, RuntimeInfo>>>,
    session_config: Arc<Mutex<Option<ddtelemetry::config::Config>>>,
    tracer_config: Arc<Mutex<tracer::Config>>,
    log_guard: Arc<Mutex<Option<(MultiEnvFilterGuard<'static>, MultiWriterGuard<'static>)>>>,
    #[cfg(feature = "tracing")]
    session_id: String,
}

impl SessionInfo {
    fn get_runtime(&self, runtime_id: &String) -> RuntimeInfo {
        let mut runtimes = self.runtimes.lock().unwrap();
        match runtimes.get(runtime_id) {
            Some(runtime) => runtime.clone(),
            None => {
                let mut runtime = RuntimeInfo::default();
                runtimes.insert(runtime_id.clone(), runtime.clone());
                #[cfg(feature = "tracing")]
                if enabled!(Level::INFO) {
                    runtime.instance_id = InstanceId {
                        session_id: self.session_id.clone(),
                        runtime_id: runtime_id.clone(),
                    };
                    info!(
                        "Registering runtime_id {} for session {}",
                        runtime_id, self.session_id
                    );
                }
                runtime
            }
        }
    }

    async fn shutdown(&self) {
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .lock()
            .unwrap()
            .drain()
            .map(|(_, instance)| instance)
            .collect();

        let runtimes_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(runtimes_shutting_down).await;
    }

    async fn shutdown_running_instances(&self) {
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, instance)| instance.clone())
            .collect();

        let instances_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(instances_shutting_down).await;
    }

    async fn shutdown_runtime(self, runtime_id: &String) {
        let runtime = match self.runtimes.lock().unwrap().remove(runtime_id) {
            Some(rt) => rt,
            None => return,
        };

        runtime.shutdown().await
    }

    fn get_telemetry_config(&self) -> MutexGuard<Option<ddtelemetry::config::Config>> {
        let mut cfg = self.session_config.lock().unwrap();

        if (*cfg).is_none() {
            *cfg = Some(ddtelemetry::config::Config::from_env())
        }

        cfg
    }

    fn modify_telemetry_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut ddtelemetry::config::Config),
    {
        if let Some(cfg) = &mut *self.get_telemetry_config() {
            f(cfg)
        }
    }

    fn get_trace_config(&self) -> MutexGuard<tracer::Config> {
        self.tracer_config.lock().unwrap()
    }

    fn modify_trace_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut tracer::Config),
    {
        f(&mut self.get_trace_config());
    }
}

#[allow(clippy::large_enum_variant)]
enum AppOrQueue {
    App(Shared<ManualFuture<(String, String)>>),
    Queue(EnqueuedTelemetryData),
}

#[allow(clippy::type_complexity)]
#[derive(Clone, Default)]
struct RuntimeInfo {
    apps: Arc<Mutex<HashMap<(String, String), Shared<ManualFuture<Option<AppInstance>>>>>>,
    app_or_actions: Arc<Mutex<HashMap<QueueId, AppOrQueue>>>,
    #[cfg(feature = "tracing")]
    instance_id: InstanceId,
}

impl RuntimeInfo {
    #[allow(clippy::type_complexity)]
    fn get_app(
        &self,
        service_name: &str,
        env_name: &str,
    ) -> (
        Shared<ManualFuture<Option<AppInstance>>>,
        Option<ManualFutureCompleter<Option<AppInstance>>>,
    ) {
        let mut apps = self.apps.lock().unwrap();
        let key = (service_name.to_owned(), env_name.to_owned());
        if let Some(found) = apps.get(&key) {
            (found.clone(), None)
        } else {
            let (future, completer) = ManualFuture::new();
            let shared = future.shared();
            apps.insert(key, shared.clone());
            (shared, Some(completer))
        }
    }

    async fn shutdown(self) {
        #[cfg(feature = "tracing")]
        info!(
            "Shutting down runtime_id {} for session {}",
            self.instance_id.runtime_id, self.instance_id.session_id
        );

        let instance_futures: Vec<_> = self
            .apps
            .lock()
            .unwrap()
            .drain()
            .map(|(_, instance)| instance)
            .collect();
        let instances: Vec<_> = join_all(instance_futures).await;
        let instances_shutting_down: Vec<_> = instances
            .into_iter()
            .map(|instance| {
                tokio::spawn(async move {
                    if let Some(instance) = instance {
                        instance
                            .telemetry
                            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop))
                            .await
                            .ok();
                        instance.telemetry_worker_shutdown.await;
                    }
                })
            })
            .collect();
        future::join_all(instances_shutting_down).await;

        #[cfg(feature = "tracing")]
        debug!(
            "Successfully shut down runtime_id {} for session {}",
            self.instance_id.runtime_id, self.instance_id.session_id
        );
    }
}

#[derive(Clone)]
struct AppInstance {
    telemetry: TelemetryWorkerHandle,
    telemetry_worker_shutdown: Shared<BoxFuture<'static, Option<()>>>,
    telemetry_metrics: Arc<Mutex<HashMap<String, ContextKey>>>,
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

struct EnqueuedTelemetryData {
    dependencies: Store<data::Dependency>,
    configurations: Store<data::Configuration>,
    integrations: Store<data::Integration>,
    metrics: Vec<MetricContext>,
    points: Vec<(String, f64, Vec<Tag>)>,
    actions: Vec<TelemetryActions>,
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

    async fn extract_telemetry_actions(&mut self, actions: &mut Vec<TelemetryActions>) {
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
                    _ = tokio::time::sleep(time::Duration::from_millis(
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
}

#[derive(Default, Clone)]
pub struct SidecarServer {
    pub trace_flusher: Arc<TraceFlusher>,
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    session_counter: Arc<Mutex<HashMap<String, u32>>>,
    pub self_telemetry_config:
        Arc<Mutex<Option<ManualFutureCompleter<ddtelemetry::config::Config>>>>,
    pub submitted_payloads: Arc<AtomicU64>,
}

impl SidecarServer {
    pub async fn accept_connection(self, async_channel: AsyncChannel) {
        let server = datadog_ipc::tarpc::server::BaseChannel::new(
            datadog_ipc::tarpc::server::Config {
                pending_response_buffer: 10000,
            },
            Transport::from(async_channel),
        );

        let mut executor = datadog_ipc::sequential::execute_sequential(
            server.requests(),
            self.clone().serve(),
            100,
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel::<_>(100);
        let tx = executor.swap_sender(tx);

        let session_counter = self.session_counter.clone();
        let submitted_payloads = self.submitted_payloads.clone();
        let session_interceptor = tokio::spawn(async move {
            let mut sessions = HashSet::new();
            let mut instances = HashSet::new();
            loop {
                let (serve, req) = match rx.recv().await {
                    None => return (sessions, instances),
                    Some(s) => s,
                };

                submitted_payloads.fetch_add(1, Ordering::Relaxed);

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
                            match session_counter.lock().unwrap().entry(session) {
                                Entry::Occupied(mut entry) => entry.insert(entry.get() + 1),
                                Entry::Vacant(entry) => *entry.insert(1),
                            };
                        }
                    }
                }
            }
        });

        if let Err(e) = executor.await {
            warn!("Error from executor: {e:?}");
        }

        if let Ok((sessions, instances)) = session_interceptor.await {
            for session in sessions {
                let stop = {
                    let mut counter = self.session_counter.lock().unwrap();
                    if let Entry::Occupied(mut entry) = counter.entry(session.clone()) {
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
                    .lock()
                    .unwrap()
                    .get(&instance_id.session_id)
                    .cloned();
                if let Some(session) = maybe_session {
                    session.shutdown_runtime(&instance_id.runtime_id).await;
                }
            }
        }
    }

    pub fn active_session_count(&self) -> usize {
        self.session_counter.lock().unwrap().len()
    }

    fn get_session(&self, session_id: &String) -> SessionInfo {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(session_id) {
            Some(session) => session.clone(),
            None => {
                let mut session = SessionInfo::default();
                #[cfg(feature = "tracing")]
                if enabled!(Level::INFO) {
                    session.session_id = session_id.clone();
                    info!("Initializing new session: {}", session_id);
                }
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
        let session = match self.sessions.lock().unwrap().remove(session_id) {
            Some(session) => session,
            None => return,
        };

        info!("Shutting down session: {}", session_id);
        session.shutdown().await;
        debug!("Successfully shut down session: {}", session_id);
    }

    async fn get_app(
        &self,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMeta,
        service_name: &str,
        env_name: &str,
        inital_actions: Vec<TelemetryActions>,
    ) -> Option<AppInstance> {
        let rt_info = self.get_runtime(instance_id);

        let (app_future, completer) = rt_info.get_app(service_name, env_name);
        if completer.is_none() {
            return app_future.await;
        }

        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            service_name.to_owned(),
            runtime_meta.language_name.clone(),
            runtime_meta.language_version.clone(),
            runtime_meta.tracer_version.clone(),
        );
        builder.runtime_id = Some(instance_id.runtime_id.clone());
        builder.application.env = Some(env_name.to_owned());
        let session_info = self.get_session(&instance_id.session_id);
        let mut config = session_info
            .session_config
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(ddtelemetry::config::Config::from_env);
        config.restartable = true;

        // TODO: log errors
        let instance_option = match builder.spawn_with_config(config.clone()).await {
            Ok((handle, worker_join)) => {
                info!("spawning telemetry worker {config:?}");

                let instance = AppInstance {
                    telemetry: handle,
                    telemetry_worker_shutdown: worker_join.map(Result::ok).boxed().shared(),
                    telemetry_metrics: Default::default(),
                };

                instance.telemetry.send_msgs(inital_actions).await.ok();

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
        completer.unwrap().complete(instance_option).await;
        app_future.await
    }

    fn send_trace_v04(&self, headers: &SerializedTracerHeaderTags, data: &[u8], target: &Endpoint) {
        let headers: TracerHeaderTags = headers.into();

        let size = data.len();
        let traces: Vec<Vec<pb::Span>> = match rmp_serde::from_slice(data) {
            Ok(res) => res,
            Err(err) => {
                error!("Error deserializing trace from request body: {err}");
                return;
            }
        };

        if traces.is_empty() {
            error!("No traces deserialized from the request body.");
            return;
        }

        let payload =
            trace_utils::collect_trace_chunks(traces, &headers, |_chunk, _root_span_index| {});

        // send trace payload to our trace flusher
        let data = SendData::new(size, payload, headers, target);
        self.trace_flusher.enqueue(data);
    }
}

type NoResponse = Ready<()>;

fn no_response() -> NoResponse {
    future::ready(())
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

impl SidecarInterface for SidecarServer {
    type PingFut = Ready<()>;

    fn ping(self, _: Context) -> Self::PingFut {
        future::ready(())
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

    type EnqueueActionsFut = NoResponse;

    fn enqueue_actions(
        self,
        _context: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) -> Self::EnqueueActionsFut {
        let rt_info = self.get_runtime(&instance_id);
        let mut queue = rt_info.app_or_actions.lock().unwrap();
        match queue.entry(queue_id) {
            Entry::Occupied(mut entry) => match entry.get_mut() {
                AppOrQueue::Queue(ref mut data) => {
                    data.process(actions);
                }
                AppOrQueue::App(service_future) => {
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
                        let app_future = if let Some(fut) = apps.lock().unwrap().get(&service) {
                            fut.clone()
                        } else {
                            return;
                        };
                        if let Some(mut app) = app_future.await {
                            let actions =
                                EnqueuedTelemetryData::process_immediately(actions, &mut app).await;
                            app.telemetry.send_msgs(actions).await.ok();
                        }
                    });
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(AppOrQueue::Queue(EnqueuedTelemetryData::processed(actions)));
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
        runtime_meta: RuntimeMeta,
        service_name: String,
        env_name: String,
    ) -> Self::RegisterServiceAndFlushQueuedActionsFut {
        // We need a channel to have enqueuing code await
        let (future, completer) = ManualFuture::new();
        let app_or_queue = {
            let rt_info = self.get_runtime(&instance_id);
            let mut app_or_actions = rt_info.app_or_actions.lock().unwrap();
            match app_or_actions.get(&queue_id) {
                Some(AppOrQueue::Queue(_)) => {
                    app_or_actions.insert(queue_id, AppOrQueue::App(future.shared()))
                }
                None => Some(AppOrQueue::Queue(EnqueuedTelemetryData::default())),
                _ => None,
            }
        };
        if let Some(AppOrQueue::Queue(mut enqueued_data)) = app_or_queue {
            tokio::spawn(async move {
                let mut actions: Vec<TelemetryActions> = vec![];
                enqueued_data.extract_telemetry_actions(&mut actions).await;

                if let Some(mut app) = self
                    .get_app(
                        &instance_id,
                        &runtime_meta,
                        &service_name,
                        &env_name,
                        actions,
                    )
                    .await
                {
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
                        self.get_runtime(&instance_id)
                            .app_or_actions
                            .lock()
                            .unwrap()
                            .remove(&queue_id);
                    }

                    app.telemetry.send_msgs(actions).await.ok();
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
        config: SessionConfig,
    ) -> Self::SetSessionConfigFut {
        let session = self.get_session(&session_id);
        session.modify_telemetry_config(|cfg| {
            let endpoint =
                get_product_endpoint(ddtelemetry::config::PROD_INTAKE_SUBDOMAIN, &config.endpoint);
            cfg.set_endpoint(endpoint).ok();
        });
        session.modify_trace_config(|cfg| {
            let endpoint = get_product_endpoint(
                datadog_trace_utils::config_utils::PROD_INTAKE_SUBDOMAIN,
                &config.endpoint,
            );
            cfg.set_endpoint(endpoint).ok();
        });
        self.trace_flusher
            .interval
            .store(config.flush_interval.as_millis() as u64, Ordering::Relaxed);
        self.trace_flusher
            .min_force_flush_size
            .store(config.force_flush_size as u32, Ordering::Relaxed);
        self.trace_flusher
            .min_force_drop_size
            .store(config.force_drop_size as u32, Ordering::Relaxed);

        session.log_guard.lock().unwrap().replace((
            log::MULTI_LOG_FILTER.add(config.log_level),
            log::MULTI_LOG_WRITER.add(config.log_file),
        ));

        if let Some(completer) = self.self_telemetry_config.lock().unwrap().take() {
            let config = session
                .session_config
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            tokio::spawn(async move {
                completer.complete(config).await;
            });
        }

        Box::pin(async move {
            session.shutdown_running_instances().await;
            no_response().await
        })
    }

    type SendTraceV04ShmFut = NoResponse;

    fn send_trace_v04_shm(
        self,
        _: Context,
        instance_id: InstanceId,
        handle: ShmHandle,
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
                        self.send_trace_v04(&headers, mapped.as_slice(), &endpoint);
                    }
                    Err(e) => error!("Failed mapping shared trace data memory: {}", e),
                }
            });
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
                self.send_trace_v04(&headers, data.as_slice(), &endpoint);
            });
        }

        no_response()
    }

    type DumpFut = Pin<Box<dyn Send + futures::Future<Output = String>>>;

    fn dump(self, _: Context) -> Self::DumpFut {
        Box::pin(crate::dump::dump())
    }
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

    use super::{
        InstanceId, QueueId, RuntimeMeta, SidecarInterfaceRequest, SidecarInterfaceResponse,
    };

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
        runtime_metadata: &RuntimeMeta,
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

    pub fn ping(transport: &mut SidecarTransport) -> io::Result<Duration> {
        let start = Instant::now();
        transport.call(SidecarInterfaceRequest::Ping {})?;

        Ok(Instant::now()
            .checked_duration_since(start)
            .unwrap_or_default())
    }
}
