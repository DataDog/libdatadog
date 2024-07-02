// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{
    remote_configs::RemoteConfigsGuard,
    telemetry::{AppInstance, AppOrQueue},
    InstanceId, QueueId,
};
use datadog_live_debugger::sender::generate_tags;
use ddcommon::tag::Tag;
use futures::{
    future::{self, join_all, Shared},
    FutureExt,
};
use manual_future::{ManualFuture, ManualFutureCompleter};
use simd_json::prelude::ArrayTrait;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};
use tracing::{debug, info};

type AppMap = HashMap<(String, String), Shared<ManualFuture<Option<AppInstance>>>>;

/// `SharedAppManualFut` is a struct that contains a shared future of an `AppInstance` and its
/// completer. The `app_future` is a shared future that may contain an `Option<AppInstance>`.
/// The `completer` is used to complete the `app_future`.
pub(crate) struct SharedAppManualFut {
    pub(crate) app_future: Shared<ManualFuture<Option<AppInstance>>>,
    pub(crate) completer: Option<ManualFutureCompleter<Option<AppInstance>>>,
}

/// `RuntimeInfo` is a struct that contains information about a runtime.
/// It contains a map of apps and a map of app or actions.
#[derive(Clone, Default)]
pub(crate) struct RuntimeInfo {
    pub(crate) apps: Arc<Mutex<AppMap>>,
    applications: Arc<Mutex<HashMap<QueueId, ActiveApplication>>>,
    #[cfg(feature = "tracing")]
    pub(crate) instance_id: InstanceId,
}

/// `ActiveApplications` is a struct the contains information about a known in flight application.
/// Telemetry lifecycles (see `app_or_actions`) and remote_config `remote_config_guard` are bound to
/// it.
/// Each app is represented by a shared future that may contain an `Option<AppInstance>`.
/// Each action is represented by an `AppOrQueue` enum. Combining apps and actions are necessary
/// because service and env names are not known until later in the initialization process.
/// Similarly, each application has its own global tags.
#[derive(Default)]
pub(crate) struct ActiveApplication {
    pub app_or_actions: AppOrQueue,
    pub remote_config_guard: Option<RemoteConfigsGuard>,
    pub env: Option<String>,
    pub app_version: Option<String>,
    pub global_tags: Vec<Tag>,
    pub live_debugger_tag_cache: Option<Arc<String>>,
}

impl RuntimeInfo {
    /// Retrieves the `AppInstance` for a given service name and environment name.
    ///
    /// # Arguments
    ///
    /// * `service_name` - A string slice that holds the name of the service.
    /// * `env_name` - A string slice that holds the name of the environment.
    ///
    /// # Returns
    ///
    /// * `SharedAppManualFut` - A struct that contains the shared future of the `AppInstance` and
    ///   its completer.
    pub(crate) fn get_app(&self, service_name: &str, env_name: &str) -> SharedAppManualFut {
        let mut apps = self.lock_apps();
        let key = (service_name.to_owned(), env_name.to_owned());
        if let Some(found) = apps.get(&key) {
            SharedAppManualFut {
                app_future: found.clone(),
                completer: None,
            }
        } else {
            let (future, completer) = ManualFuture::new();
            let shared = future.shared();
            apps.insert(key, shared.clone());
            SharedAppManualFut {
                app_future: shared,
                completer: Some(completer),
            }
        }
    }
    /// Shuts down the runtime.
    /// This involves shutting down all the instances in the runtime.
    pub(crate) async fn shutdown(self) {
        #[cfg(feature = "tracing")]
        info!(
            "Shutting down runtime_id {} for session {}",
            self.instance_id.runtime_id, self.instance_id.session_id
        );

        let instance_futures: Vec<_> = self
            .lock_apps()
            .drain()
            .map(|(_, instance)| instance)
            .collect();
        let instances: Vec<_> = join_all(instance_futures).await;
        let instances_shutting_down: Vec<_> = instances
            .into_iter()
            .map(|instance| {
                tokio::spawn(async move {
                    if let Some(instance) = instance {
                        drop(instance.telemetry); // start shutdown
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

    // TODO: APMSP-1076 Investigate if we can encapsulate the stats computation functionality so we
    // don't have to expose apps publicly.
    /// Locks the apps map and returns a mutable reference to it.
    ///
    /// # Returns
    ///
    /// * `<MutexGuard<AppMap>>` - A mutable reference to the apps map.
    pub(crate) fn lock_apps(&self) -> MutexGuard<AppMap> {
        self.apps.lock().unwrap()
    }

    /// Locks the applications map and returns a mutable reference to it.
    ///
    /// # Returns
    ///
    /// * `MutexGuard<HashMap<QueueId, ActiveApplications>>` - A mutable reference to the
    ///   applications map.
    pub(crate) fn lock_applications(&self) -> MutexGuard<HashMap<QueueId, ActiveApplication>> {
        self.applications.lock().unwrap()
    }

    /// Sets the cached debugger tags if not set and returns them.
    ///
    /// # Arguments
    ///
    /// * `debugger_version` - The version of the live debugger to report.
    /// * `queue_id` - The unique identifier for the trace context.
    ///
    /// # Returns
    ///
    /// * `Arc<String>` - A percent encoded string to be passed to
    ///   datadog_live_debugger::sender::send.
    pub fn get_debugger_tags(
        &mut self,
        debugger_version: &dyn Display,
        queue_id: QueueId,
    ) -> Arc<String> {
        if let Some(app) = self.lock_applications().get_mut(&queue_id) {
            if let Some(env) = &app.env {
                if let Some(version) = &app.app_version {
                    let tags = Arc::new(generate_tags(
                        debugger_version,
                        env,
                        version,
                        &self.instance_id.runtime_id,
                        &mut app.global_tags.iter(),
                    ));
                    app.live_debugger_tag_cache = Some(tags.clone());
                    return tags;
                }
            }
        }
        Arc::new(format!("debugger_version:{debugger_version}"))
    }
}

impl ActiveApplication {
    /// Sets the cached debugger tags if not set and returns them.
    ///
    /// # Arguments
    ///
    /// * `env` - The environment of the current application.
    /// * `app_version` - The version of the current application.
    /// * `global_tags` - The global tags of the current application.
    pub fn set_metadata(&mut self, env: String, app_version: String, global_tags: Vec<Tag>) {
        self.env = Some(env);
        self.app_version = Some(app_version);
        self.global_tags = global_tags;
        self.live_debugger_tag_cache = None;
    }
}

// TODO: APM-1079 - Add unit tests for RuntimeInfo
