// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{InstanceId, RuntimeMetadata, SidecarAction, SidecarServer};
use anyhow::{anyhow, Result};
use ddcommon::MutexExt;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::one_way_shared_memory::OneWayShmWriter;
use crate::primary_sidecar_identifier;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::platform::NamedShmHandle;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use zwohash::ZwoHasher;

use ddcommon::tag::Tag;
use ddtelemetry::worker::TelemetryWorkerBuilder;
use serde::Deserialize;
use std::ops::Sub;
use std::sync::LazyLock;
use std::time::SystemTime;

use ddtelemetry::config::Config;
use ddtelemetry::data::{self, Integration};
use ddtelemetry::metrics::{ContextKey, MetricContext};
use ddtelemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerHandle};
use manual_future::ManualFuture;
use serde_with::{serde_as, VecSkipError};
use tokio::time::sleep;

type ComposerCache = HashMap<PathBuf, (SystemTime, Arc<Vec<data::Dependency>>)>;

static COMPOSER_CACHE: LazyLock<tokio::sync::Mutex<ComposerCache>> =
    LazyLock::new(|| tokio::sync::Mutex::new(Default::default()));

static LAST_CACHE_CLEAN: AtomicU64 = AtomicU64::new(0);

static TELEMETRY_ACTION_SENDER: OnceLock<mpsc::Sender<InternalTelemetryActions>> = OnceLock::new();

#[serde_as]
#[derive(Deserialize)]
struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    packages: Vec<data::Dependency>,
}

pub struct TelemetryCachedEntry {
    last_used: Instant,
    pub client: Arc<Mutex<TelemetryCachedClient>>,
}

pub struct TelemetryCachedClient {
    pub worker: TelemetryWorkerHandle,
    pub shm_writer: OneWayShmWriter<NamedShmHandle>,
    pub config_sent: bool,
    pub buffered_integrations: HashSet<Integration>,
    pub buffered_composer_paths: HashSet<PathBuf>,
    pub telemetry_metrics: HashMap<String, ContextKey>,
    pub handle: Option<JoinHandle<()>>,
}

impl TelemetryCachedClient {
    fn new(
        service: &str,
        env: &str,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMetadata,
        get_config: impl FnOnce() -> ddtelemetry::config::Config,
    ) -> Self {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            service.to_string(),
            runtime_meta.language_name.to_string(),
            runtime_meta.language_version.to_string(),
            runtime_meta.tracer_version.to_string(),
        );

        builder.runtime_id = Some(instance_id.runtime_id.clone());
        builder.application.env = Some(env.to_string());
        let config = get_config();
        builder.config = config.clone();

        let (handle, _join) = builder.spawn();

        info!("spawning telemetry worker {config:?}");
        Self {
            worker: handle.clone(),
            shm_writer: {
                #[allow(clippy::unwrap_used)]
                OneWayShmWriter::<NamedShmHandle>::new(path_for_telemetry(service, env)).unwrap()
            },
            config_sent: false,
            buffered_integrations: HashSet::new(),
            buffered_composer_paths: HashSet::new(),
            telemetry_metrics: Default::default(),
            handle: None,
        }
    }

    pub fn write_shm_file(&self) {
        if let Ok(buf) = bincode::serialize(&(
            &self.config_sent,
            &self.buffered_integrations,
            &self.buffered_composer_paths,
        )) {
            self.shm_writer.write(&buf);
        } else {
            warn!("Failed to serialize telemetry data for shared memory");
        }
    }

    pub fn register_metric(&mut self, metric: MetricContext) {
        if !self.telemetry_metrics.contains_key(&metric.name) {
            self.telemetry_metrics.insert(
                metric.name.clone(),
                self.worker.register_metric_context(
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
        #[allow(clippy::unwrap_used)]
        TelemetryActions::AddPoint((val, *self.telemetry_metrics.get(&name).unwrap(), tags))
    }

    pub fn process_actions(
        &mut self,
        sidecar_actions: Vec<SidecarAction>,
    ) -> Vec<TelemetryActions> {
        let mut actions = vec![];
        for action in sidecar_actions {
            match action {
                SidecarAction::Telemetry(t) => actions.push(t),
                SidecarAction::RegisterTelemetryMetric(metric) => self.register_metric(metric),
                SidecarAction::AddTelemetryMetricPoint(point) => {
                    actions.push(self.to_telemetry_point(point));
                }
                SidecarAction::PhpComposerTelemetryFile(_) => {} // handled separately
                SidecarAction::ClearQueueId => {}                // handled separately
            }
        }
        actions
    }

    pub async fn process_composer_paths(paths: Vec<PathBuf>) -> Vec<TelemetryActions> {
        let mut result = vec![];

        for path in paths {
            let deps = Self::extract_composer_telemetry(path).await;
            result.extend(deps.iter().cloned().map(TelemetryActions::AddDependency));
        }

        result
    }

    pub fn extract_composer_telemetry(path: PathBuf) -> ManualFuture<Arc<Vec<data::Dependency>>> {
        let (deps, completer) = ManualFuture::new();
        tokio::spawn(async {
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
                    async fn parse(path: &PathBuf) -> anyhow::Result<Vec<data::Dependency>> {
                        let mut json = tokio::fs::read(&path).await?;
                        #[cfg(not(target_arch = "x86"))]
                        let parsed: ComposerPackages = simd_json::from_slice(json.as_mut_slice())?;
                        #[cfg(target_arch = "x86")]
                        let parsed = crate::interface::ComposerPackages { packages: vec![] }; // not interested in 32 bit
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

impl Drop for TelemetryCachedClient {
    fn drop(&mut self) {
        self.shm_writer.write(&[]);
    }
}

type ServiceString = String;
type EnvString = String;
type TelemetryCachedClientKey = (ServiceString, EnvString);

pub struct TelemetryCachedClientSet {
    pub inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedEntry>>>,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for TelemetryCachedClientSet {
    fn default() -> Self {
        let inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedEntry>>> =
            Arc::new(Default::default());
        let clients = inner.clone();

        let handle = tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(60)).await;
                let mut lock = clients.lock_or_panic();
                lock.retain(|_, c| c.last_used.elapsed() < Duration::from_secs(1800));
            }
        });

        Self {
            inner,
            cleanup_handle: Some(handle),
        }
    }
}

impl Drop for TelemetryCachedClientSet {
    fn drop(&mut self) {
        if let Some(handle) = self.cleanup_handle.take() {
            handle.abort();
        }
    }
}

impl Clone for TelemetryCachedClientSet {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            cleanup_handle: None,
        }
    }
}

impl TelemetryCachedClientSet {
    pub fn get_or_create<F>(
        &self,
        service: &str,
        env: &str,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMetadata,
        get_config: F,
    ) -> Arc<Mutex<TelemetryCachedClient>>
    where
        F: FnOnce() -> ddtelemetry::config::Config,
    {
        let key = (service.to_string(), env.to_string());

        let mut map = self.inner.lock_or_panic();

        if let Some(existing) = map.get_mut(&key) {
            existing.last_used = Instant::now();
            tokio::spawn({
                let worker = existing.client.lock_or_panic().worker.clone();
                async move {
                    worker
                        .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
                        .await
                        .ok();
                }
            });

            info!("Reusing existing telemetry client for {key:?}");
            return existing.client.clone();
        }

        let entry = TelemetryCachedEntry {
            last_used: Instant::now(),
            client: Arc::new(Mutex::new(TelemetryCachedClient::new(
                service,
                env,
                instance_id,
                runtime_meta,
                get_config,
            ))),
        };

        let entry = map.entry(key.clone()).or_insert(entry);

        tokio::spawn({
            let worker = entry.client.lock_or_panic().worker.clone();
            async move {
                worker
                    .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
                    .await
                    .ok();
            }
        });

        info!("Created new telemetry client for {key:?}");

        entry.client.clone()
    }

    pub fn remove_telemetry_client(&self, service: &str, env: &str) {
        let key = (service.to_string(), env.to_string());
        self.inner.lock_or_panic().remove(&key);
    }
}

pub fn path_for_telemetry(service: &str, env: &str) -> CString {
    let mut hasher = ZwoHasher::default();
    service.hash(&mut hasher);
    env.hash(&mut hasher);
    let hash = hasher.finish();

    let mut path = format!(
        "/ddtl{}-{}",
        primary_sidecar_identifier(),
        BASE64_URL_SAFE_NO_PAD.encode(hash.to_ne_bytes()),
    );
    path.truncate(31);

    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

#[derive(Debug)]
pub struct InternalTelemetryActions {
    pub instance_id: InstanceId,
    pub service_name: String,
    pub env_name: String,
    pub actions: Vec<TelemetryActions>,
}

pub fn get_telemetry_action_sender() -> Result<mpsc::Sender<InternalTelemetryActions>> {
    TELEMETRY_ACTION_SENDER
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("Telemetry action sender not initialized"))
}

pub(crate) async fn telemetry_action_receiver_task(sidecar: SidecarServer) {
    info!("Starting telemetry action receiver task...");

    // create mpsc pair and set TELEMETRY_ACTION_SENDER
    let (tx, mut rx) = mpsc::channel(1000);
    if TELEMETRY_ACTION_SENDER.set(tx).is_err() {
        warn!("Failed to set telemetry action sender");
        return;
    }

    while let Some(actions) = rx.recv().await {
        let telemetry_client = get_telemetry_client(
            &sidecar,
            &actions.instance_id,
            &actions.service_name,
            &actions.env_name,
        );
        let client = telemetry_client.lock_or_panic().worker.clone();

        for action in actions.actions {
            let action_str = format!("{action:?}");
            match client.send_msg(action).await {
                Ok(_) => {
                    debug!("Sent telemetry action to TelemetryWorker: {action_str}");
                }
                Err(e) => {
                    warn!("Failed to send telemetry action {action_str} to TelemetryWorker: {e}");
                }
            }
        }
    }
    info!("Telemetry action receiver task shutting down.");
}

fn get_telemetry_client(
    sidecar: &SidecarServer,
    instance_id: &InstanceId,
    service_name: &str,
    env_name: &str,
) -> Arc<Mutex<TelemetryCachedClient>> {
    let session = sidecar.get_session(&instance_id.session_id);
    let trace_config = session.get_trace_config();
    let runtime_meta = RuntimeMetadata::new(
        trace_config.language.as_str(),
        trace_config.language_version.as_str(),
        trace_config.tracer_version.as_str(),
    );

    let get_config = || {
        sidecar
            .get_session(&instance_id.session_id)
            .session_config
            .lock_or_panic()
            .as_ref()
            .cloned()
            .unwrap_or_else(|| {
                warn!("Failed to get telemetry session config for {instance_id:?}.");
                Config::default()
            })
    };

    TelemetryCachedClientSet::get_or_create(
        &sidecar.telemetry_clients,
        service_name,
        env_name,
        instance_id,
        &runtime_meta,
        get_config,
    )
}
