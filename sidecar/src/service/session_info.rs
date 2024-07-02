// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::AtomicI32;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use datadog_remote_config::fetch::ConfigInvariants;
use futures::future;
use tracing::info;

use crate::log::{MultiEnvFilterGuard, MultiWriterGuard};
use crate::tracer;

use crate::service::{InstanceId, RuntimeInfo};
/// `SessionInfo` holds information about a session.
///
/// It contains a list of runtimes, session configuration, tracer configuration, and log guards.
/// It also has methods to manage the runtimes and configurations.
#[derive(Default)]
pub(crate) struct SessionInfo {
    runtimes: Arc<Mutex<HashMap<String, RuntimeInfo>>>,
    pub(crate) session_config: Arc<Mutex<Option<ddtelemetry::config::Config>>>,
    debugger_config: Arc<Mutex<datadog_live_debugger::sender::Config>>,
    tracer_config: Arc<Mutex<tracer::Config>>,
    dogstatsd: Arc<Mutex<Option<dogstatsd_client::Client>>>,
    remote_config_invariants: Arc<Mutex<Option<ConfigInvariants>>>,
    #[cfg(windows)]
    pub(crate) remote_config_notify_function:
        Arc<Mutex<crate::service::remote_configs::RemoteConfigNotifyFunction>>,
    pub(crate) log_guard:
        Arc<Mutex<Option<(MultiEnvFilterGuard<'static>, MultiWriterGuard<'static>)>>>,
    pub(crate) session_id: String,
    pub(crate) pid: Arc<AtomicI32>,
}

impl Clone for SessionInfo {
    fn clone(&self) -> Self {
        SessionInfo {
            runtimes: self.runtimes.clone(),
            session_config: self.session_config.clone(),
            debugger_config: self.debugger_config.clone(),
            tracer_config: self.tracer_config.clone(),
            dogstatsd: self.dogstatsd.clone(),
            remote_config_invariants: self.remote_config_invariants.clone(),
            #[cfg(windows)]
            remote_config_notify_function: self.remote_config_notify_function.clone(),
            log_guard: self.log_guard.clone(),
            session_id: self.session_id.clone(),
            pid: self.pid.clone(),
        }
    }
}

impl SessionInfo {
    /// Returns the `RuntimeInfo` for a given runtime ID.
    ///
    /// If the runtime does not exist, it creates a new one and returns it.
    ///
    /// # Arguments
    ///
    /// * `runtime_id` - The ID of the runtime.
    // TODO: APM-1076 This function should either be refactored or have its name changed as its
    // performing a get or create operation.
    pub(crate) fn get_runtime(&self, runtime_id: &String) -> RuntimeInfo {
        let mut runtimes = self.lock_runtimes();
        match runtimes.get(runtime_id) {
            Some(runtime) => runtime.clone(),
            None => {
                let mut runtime = RuntimeInfo::default();
                runtime.instance_id = InstanceId {
                    session_id: self.session_id.clone(),
                    runtime_id: runtime_id.clone(),
                };
                runtimes.insert(runtime_id.clone(), runtime.clone());
                info!(
                    "Registering runtime_id {} for session {}",
                    runtime_id, self.session_id
                );
                runtime
            }
        }
    }

    /// Shuts down all runtimes in the session.
    pub(crate) async fn shutdown(&self) {
        let runtimes: Vec<RuntimeInfo> = self
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
    pub(crate) async fn shutdown_running_instances(&self) {
        let runtimes: Vec<RuntimeInfo> = self
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
    pub(crate) async fn shutdown_runtime(&self, runtime_id: &str) {
        let maybe_runtime = {
            let mut runtimes = self.lock_runtimes();
            runtimes.remove(runtime_id)
        };

        if let Some(runtime) = maybe_runtime {
            runtime.shutdown().await;
        }
    }

    pub(crate) fn lock_runtimes(&self) -> MutexGuard<HashMap<String, RuntimeInfo>> {
        self.runtimes.lock().unwrap()
    }

    pub(crate) fn get_telemetry_config(&self) -> MutexGuard<Option<ddtelemetry::config::Config>> {
        let mut cfg = self.session_config.lock().unwrap();

        if (*cfg).is_none() {
            *cfg = Some(ddtelemetry::config::Config::from_env())
        }

        cfg
    }

    pub(crate) fn modify_telemetry_config<F>(&self, f: F)
    where
        F: FnOnce(&mut ddtelemetry::config::Config),
    {
        if let Some(cfg) = &mut *self.get_telemetry_config() {
            f(cfg)
        }
    }

    pub(crate) fn get_trace_config(&self) -> MutexGuard<tracer::Config> {
        self.tracer_config.lock().unwrap()
    }

    pub(crate) fn modify_trace_config<F>(&self, f: F)
    where
        F: FnOnce(&mut tracer::Config),
    {
        f(&mut self.get_trace_config());
    }

    pub(crate) fn get_dogstatsd(&self) -> MutexGuard<Option<dogstatsd_client::Client>> {
        self.dogstatsd.lock().unwrap()
    }

    pub(crate) fn configure_dogstatsd<F>(&self, f: F)
    where
        F: FnOnce(&mut Option<dogstatsd_client::Client>),
    {
        f(&mut self.get_dogstatsd());
    }

    pub fn get_debugger_config(&self) -> MutexGuard<datadog_live_debugger::sender::Config> {
        self.debugger_config.lock().unwrap()
    }

    pub fn modify_debugger_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut datadog_live_debugger::sender::Config),
    {
        f(&mut self.get_debugger_config());
    }

    pub fn set_remote_config_invariants(&self, invariants: ConfigInvariants) {
        *self.remote_config_invariants.lock().unwrap() = Some(invariants);
    }

    pub fn get_remote_config_invariants(&self) -> MutexGuard<Option<ConfigInvariants>> {
        self.remote_config_invariants.lock().unwrap()
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
