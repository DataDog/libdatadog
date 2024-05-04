use crate::interface::{AppInstance, AppOrQueue};
use crate::service::{InstanceId, QueueId};
use ddtelemetry::worker::{LifecycleAction, TelemetryActions};
use futures::{
    future::{self, join_all, Shared},
    FutureExt,
};
use manual_future::{ManualFuture, ManualFutureCompleter};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tracing::{debug, info};

#[allow(clippy::type_complexity)]
#[derive(Clone, Default)]
pub struct RuntimeInfo {
    pub(crate) apps:
        Arc<Mutex<HashMap<(String, String), Shared<ManualFuture<Option<AppInstance>>>>>>,
    app_or_actions: Arc<Mutex<HashMap<QueueId, AppOrQueue>>>,
    #[cfg(feature = "tracing")]
    pub instance_id: InstanceId,
}

impl RuntimeInfo {
    #[allow(clippy::type_complexity)]
    pub(crate) fn get_app(
        &self,
        service_name: &str,
        env_name: &str,
    ) -> (
        Shared<ManualFuture<Option<AppInstance>>>,
        Option<ManualFutureCompleter<Option<AppInstance>>>,
    ) {
        let mut apps = self.lock_apps();
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

    pub async fn shutdown(self) {
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

    #[allow(clippy::type_complexity)]
    pub(crate) fn lock_apps(
        &self,
    ) -> MutexGuard<HashMap<(String, String), Shared<ManualFuture<Option<AppInstance>>>>> {
        self.apps.lock().unwrap()
    }

    pub(crate) fn lock_app_or_actions(&self) -> MutexGuard<HashMap<QueueId, AppOrQueue>> {
        self.app_or_actions.lock().unwrap()
    }
}
