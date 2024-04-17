// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use futures::future;

use tracing::{enabled, info, Level};

use crate::log::{MultiEnvFilterGuard, MultiWriterGuard};
use crate::tracer;

use crate::service::InstanceId;
/// `SessionInfo` holds information about a session.
///
/// It contains a list of runtimes, session configuration, tracer configuration, and log guards.
/// It also has methods to manage the runtimes and configurations.
#[derive(Default, Clone)]
pub struct SessionInfo {
    runtimes: Arc<Mutex<HashMap<String, crate::interface::RuntimeInfo>>>,
    pub session_config: Arc<Mutex<Option<ddtelemetry::config::Config>>>,
    tracer_config: Arc<Mutex<tracer::Config>>,
    pub log_guard: Arc<Mutex<Option<(MultiEnvFilterGuard<'static>, MultiWriterGuard<'static>)>>>,
    #[cfg(feature = "tracing")]
    pub session_id: String,
}

impl SessionInfo {
    /// Returns the `RuntimeInfo` for a given runtime ID.
    ///
    /// If the runtime does not exist, it creates a new one and returns it.
    ///
    /// # Arguments
    ///
    /// * `runtime_id` - The ID of the runtime.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::SessionInfo;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let session_info = SessionInfo::default();
    ///     let runtime_info = session_info.get_runtime(&"runtime1".to_string());
    /// }
    /// ```
    // DEV-TODO: This function should likely either be refactored or have its name changed as its
    // performing a get or insert operation.
    pub fn get_runtime(&self, runtime_id: &String) -> crate::interface::RuntimeInfo {
        let mut runtimes = self.lock_runtimes();
        match runtimes.get(runtime_id) {
            Some(runtime) => runtime.clone(),
            None => {
                let mut runtime = crate::interface::RuntimeInfo::default();
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

    /// Shuts down all runtimes in the session.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::SessionInfo;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let session_info = SessionInfo::default();
    ///     session_info.shutdown().await;
    /// }
    /// ```
    pub async fn shutdown(&self) {
        let runtimes: Vec<crate::interface::RuntimeInfo> = self
            .lock_runtimes()
            .drain()
            .map(|(_, instance)| instance)
            .collect();

        let runtimes_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(runtimes_shutting_down).await;
    }
    /// Shuts down all running instances in the session.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::SessionInfo;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let session_info = SessionInfo::default();
    ///     session_info.shutdown_running_instances().await;
    /// }
    /// ```
    pub async fn shutdown_running_instances(&self) {
        let runtimes: Vec<crate::interface::RuntimeInfo> = self
            .lock_runtimes()
            .drain()
            .map(|(_, instance)| instance)
            .collect();

        let instances_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(instances_shutting_down).await;
    }

    /// Shuts down a specific runtime in the session.
    ///
    /// # Arguments
    ///
    /// * `runtime_id` - The ID of the runtime.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::SessionInfo;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let session_info = SessionInfo::default();
    ///     session_info.shutdown_runtime(&"runtime1".to_string()).await;
    /// }
    /// ```
    pub async fn shutdown_runtime(&self, runtime_id: &String) {
        let runtime = match self.lock_runtimes().remove(runtime_id) {
            Some(rt) => rt,
            None => return,
        };

        runtime.shutdown().await
    }

    pub fn lock_runtimes(&self) -> MutexGuard<HashMap<String, crate::interface::RuntimeInfo>> {
        self.runtimes.lock().unwrap()
    }

    pub fn get_telemetry_config(&self) -> MutexGuard<Option<ddtelemetry::config::Config>> {
        let mut cfg = self.session_config.lock().unwrap();

        if (*cfg).is_none() {
            *cfg = Some(ddtelemetry::config::Config::from_env())
        }

        cfg
    }

    pub fn modify_telemetry_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut ddtelemetry::config::Config),
    {
        if let Some(cfg) = &mut *self.get_telemetry_config() {
            f(cfg)
        }
    }

    pub fn get_trace_config(&self) -> MutexGuard<tracer::Config> {
        self.tracer_config.lock().unwrap()
    }

    pub fn modify_trace_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut tracer::Config),
    {
        f(&mut self.get_trace_config());
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_runtime() {
        let session_info = SessionInfo::default();
        let runtime_id = "runtime1".to_string();

        // Test that a new runtime is created if it doesn't exist
        let _ = session_info.get_runtime(&runtime_id);
        assert!(session_info
            .runtimes
            .lock()
            .unwrap()
            .contains_key(&runtime_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_shutdown() {
        let session_info = SessionInfo::default();
        session_info.get_runtime(&"runtime1".to_string());

        // Test that all runtimes are shut down
        session_info.shutdown().await;
        assert!(session_info.runtimes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_shutdown_running_instances() {
        let session_info = SessionInfo::default();
        session_info.get_runtime(&"runtime1".to_string());

        // Test that all running instances are shut down
        session_info.shutdown_running_instances().await;
        assert!(session_info.runtimes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_shutdown_runtime() {
        let session_info = SessionInfo::default();
        let runtime_id1 = "runtime1".to_string();
        let runtime_id2 = "runtime2".to_string();
        session_info.get_runtime(&runtime_id1);
        session_info.get_runtime(&runtime_id2);

        session_info.shutdown_runtime(&runtime_id1).await;
        assert!(!session_info
            .runtimes
            .lock()
            .unwrap()
            .contains_key(&runtime_id1));
        assert!(session_info
            .runtimes
            .lock()
            .unwrap()
            .contains_key(&runtime_id2));
    }
}
