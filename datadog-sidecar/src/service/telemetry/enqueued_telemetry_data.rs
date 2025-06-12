// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddtelemetry::data;
use ddtelemetry::worker::TelemetryActions;
use manual_future::ManualFuture;
use serde::Deserialize;
use serde_with::{serde_as, VecSkipError};
use std::collections::HashMap;
use std::ops::Sub;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;

use crate::service::telemetry::AppInstance;
use crate::service::SidecarAction;

type ComposerCache = HashMap<PathBuf, (SystemTime, Arc<Vec<data::Dependency>>)>;

static COMPOSER_CACHE: LazyLock<tokio::sync::Mutex<ComposerCache>> =
    LazyLock::new(|| tokio::sync::Mutex::new(Default::default()));

static LAST_CACHE_CLEAN: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));

#[serde_as]
#[derive(Deserialize)]
struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    packages: Vec<data::Dependency>,
}

/// Processes a vector of `SidecarAction` immediately and returns a vector of
/// `TelemetryActions`.
///
/// # Arguments
///
/// * `sidecar_actions` - A vector of `SidecarAction` that needs to be processed immediately.
/// * `app` - A mutable reference to an `AppInstance`.
///
/// # Returns
///
/// * A vector of `TelemetryActions` that resulted from the immediate processing.
pub async fn process_immediately(
    sidecar_actions: Vec<SidecarAction>,
    app: &mut AppInstance,
) -> Vec<TelemetryActions> {
    let mut actions = vec![];
    for action in sidecar_actions {
        match action {
            SidecarAction::Telemetry(t) => actions.push(t),
            SidecarAction::PhpComposerTelemetryFile(path) => {
                for nested in extract_composer_telemetry(path).await.iter() {
                    actions.push(TelemetryActions::AddDependency(nested.clone()));
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

/// Parses and extracts telemetry data from a vendor/composer/installed.json file and returns a
/// future of the data. The data is cached for a short period of time to avoid unnecessary
/// parsing.
///
/// # Arguments
///
/// * `path` - A `PathBuf` that represents the path to the composer file.
///
/// # Returns
///
/// * A `ManualFuture` that resolves to an `Arc<Vec<data::Dependency>>>`.
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
                let last_clean = get_last_cache_clean().load(Ordering::Relaxed);
                let now_secs = Instant::now().elapsed().as_secs();
                if now_secs > last_clean + CACHE_INTERVAL
                    && get_last_cache_clean()
                        .compare_exchange(last_clean, now_secs, Ordering::SeqCst, Ordering::Acquire)
                        .is_ok()
                {
                    cache.retain(|_, (inserted, _)| {
                        *inserted > now.sub(Duration::from_secs(CACHE_INTERVAL))
                    });
                }
                packages
            }
        }
        for d in self.dependencies.unflushed() {
            actions.push(TelemetryActions::AddDependency(d.clone()));
        }
        for c in self.configurations.unflushed() {
            actions.push(TelemetryActions::AddConfig(c.clone()));
        }
        for i in self.integrations.unflushed() {
            actions.push(TelemetryActions::AddIntegration(i.clone()));
        }
    }

    /// Processes a vector of `SidecarAction` immediately and returns a vector of
    /// `TelemetryActions`.
    ///
    /// # Arguments
    ///
    /// * `sidecar_actions` - A vector of `SidecarAction` that needs to be processed immediately.
    /// * `app` - A mutable reference to an `AppInstance`.
    ///
    /// # Returns
    ///
    /// * A vector of `TelemetryActions` that resulted from the immediate processing.
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
                        actions.push(TelemetryActions::AddDependency(nested.clone()));
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

    /// Parses and extracts telemetry data from a vendor/composer/installed.json file and returns a
    /// future of the data. The data is cached for a short period of time to avoid unnecessary
    /// parsing.
    ///
    /// # Arguments
    ///
    /// * `path` - A `PathBuf` that represents the path to the composer file.
    ///
    /// # Returns
    ///
    /// * A `ManualFuture` that resolves to an `Arc<Vec<data::Dependency>>>`.
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

    /// Returns the statistics of the stored telemetry data.
    ///
    /// # Returns
    ///
    /// * An instance of `EnqueuedTelemetryStats` that represents the statistics of the stored
    ///   telemetry data.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_extract_composer_telemetry() {
        let data = extract_composer_telemetry(
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
}

//TODO: APMSP-1079 - Add more comprehensive tests for EnqueuedTelemetryData
