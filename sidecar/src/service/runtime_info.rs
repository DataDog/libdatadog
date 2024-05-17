// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{
    telemetry::{AppInstance, AppOrQueue},
    InstanceId, QueueId,
};
use ddtelemetry::worker::{LifecycleAction, TelemetryActions};
use futures::{
    future::{self, join_all, Shared},
    FutureExt,
};
use manual_future::{ManualFuture, ManualFutureCompleter};
use std::collections::HashMap;
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
/// Each app is represented by a shared future that may contain an `Option<AppInstance>`.
/// Each action is represented by an `AppOrQueue` enum. Combining apps and actions are necessary
/// because service and env names are not known until later in the initialization process.
#[derive(Clone, Default)]
pub(crate) struct RuntimeInfo {
    pub(crate) apps: Arc<Mutex<AppMap>>,
    app_or_actions: Arc<Mutex<HashMap<QueueId, AppOrQueue>>>,
    #[cfg(feature = "tracing")]
    pub(crate) instance_id: InstanceId,
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

    /// Locks the app or actions map and returns a mutable reference to it.
    ///
    /// # Returns
    ///
    /// * `MutexGuard<HashMap<QueueId, AppOrQueue>>` - A mutable reference to the app or actions
    ///   map.
    pub(crate) fn lock_app_or_actions(&self) -> MutexGuard<HashMap<QueueId, AppOrQueue>> {
        self.app_or_actions.lock().unwrap()
    }
}

// TODO: APM-1079 - Add unit tests for RuntimeInfo
