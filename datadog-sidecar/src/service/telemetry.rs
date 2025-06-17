// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};
use std::ops::Sub;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ddcommon::{tag::Tag, MutexExt};
use ddtelemetry::data::{self, Integration};
use ddtelemetry::metrics::{ContextKey, MetricContext};
use ddtelemetry::worker::{TelemetryActions, TelemetryWorkerHandle};
use manual_future::ManualFuture;
use serde::Deserialize;
use serde_with::{serde_as, VecSkipError};
use tokio::time::sleep;
use tracing::warn;

use crate::service::SidecarAction;

//
// ──────────────────────────────────────────────
//   ⬇ Type aliases and statics
// ──────────────────────────────────────────────
//

type ComposerCache = HashMap<PathBuf, (SystemTime, Arc<Vec<data::Dependency>>)>;
static COMPOSER_CACHE: OnceLock<tokio::sync::Mutex<ComposerCache>> = OnceLock::new();
static LAST_CACHE_CLEAN: OnceLock<AtomicU64> = OnceLock::new();

fn get_composer_cache() -> &'static tokio::sync::Mutex<ComposerCache> {
    COMPOSER_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

fn get_last_cache_clean() -> &'static AtomicU64 {
    LAST_CACHE_CLEAN.get_or_init(|| AtomicU64::new(0))
}

#[serde_as]
#[derive(Deserialize)]
struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    packages: Vec<data::Dependency>,
}

//
// ──────────────────────────────────────────────
//  Telemetry Client Structs
// ──────────────────────────────────────────────
//

#[derive(Clone)]
pub struct TelemetryCachedClient {
    pub client: TelemetryWorkerHandle,
    pub shmem_file: PathBuf,
    pub last_used: Instant,
    pub buffered_integrations: HashSet<Integration>,
    pub buffered_composer_paths: HashSet<PathBuf>,
    pub telemetry_metrics: Arc<Mutex<HashMap<String, ContextKey>>>,
}

impl TelemetryCachedClient {
    pub fn register_metric(&self, metric: MetricContext) {
        let mut metrics = self.telemetry_metrics.lock_or_panic();
        if !metrics.contains_key(&metric.name) {
            metrics.insert(
                metric.name.clone(),
                self.client.register_metric_context(
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
        TelemetryActions::AddPoint((
            val,
            *self.telemetry_metrics.lock_or_panic().get(&name).unwrap(),
            tags,
        ))
    }

    pub fn process_actions(&self, sidecar_actions: Vec<SidecarAction>) -> Vec<TelemetryActions> {
        let mut actions = vec![];
        for action in sidecar_actions {
            match action {
                SidecarAction::Telemetry(t) => actions.push(t),
                SidecarAction::RegisterTelemetryMetric(metric) => self.register_metric(metric),
                SidecarAction::AddTelemetryMetricPoint(point) => {
                    actions.push(self.to_telemetry_point(point));
                }
                SidecarAction::PhpComposerTelemetryFile(_) => {} // handled separately
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
            let mut cache = get_composer_cache().lock().await;
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
                        let mut json = tokio::fs::read(path).await?;
                        #[cfg(not(target_arch = "x86"))]
                        let parsed: ComposerPackages = simd_json::from_slice(json.as_mut_slice())?;
                        #[cfg(target_arch = "x86")]
                        let parsed = crate::interface::ComposerPackages { packages: vec![] };
                        Ok(parsed.packages)
                    }

                    let packages = Arc::new(parse(&path).await.unwrap_or_else(|e| {
                        warn!("Failed to report dependencies from {path:?}: {e:?}");
                        vec![]
                    }));

                    cache.insert(path, (now, packages.clone()));

                    // Periodic cleanup
                    const CACHE_INTERVAL: u64 = 2000;
                    let last_clean = get_last_cache_clean().load(Ordering::Relaxed);
                    let now_secs = Instant::now().elapsed().as_secs();

                    if now_secs > last_clean + CACHE_INTERVAL
                        && get_last_cache_clean()
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

type ServiceString = String;
type EnvString = String;
type VersionString = String;
type TelemetryCachedClientKey = (ServiceString, EnvString, VersionString);

pub struct TelemetryCachedClientSet {
    pub inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedClient>>>,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for TelemetryCachedClientSet {
    fn default() -> Self {
        let inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedClient>>> =
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
