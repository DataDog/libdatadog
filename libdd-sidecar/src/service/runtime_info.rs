// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{remote_configs::RemoteConfigsGuard, InstanceId, QueueId};
use datadog_live_debugger::sender::{generate_tags, PayloadSender};
use ddcommon::{tag::Tag, MutexExt};
use simd_json::prelude::ArrayTrait;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};
use tracing::{debug, info};

/// `RuntimeInfo` is a struct that contains information about a runtime.
/// It contains a map of apps and a map of app or actions.
#[derive(Clone, Default)]
pub(crate) struct RuntimeInfo {
    applications: Arc<Mutex<HashMap<QueueId, ActiveApplication>>>,
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
    pub remote_config_guard: Option<RemoteConfigsGuard>,
    pub env: Option<String>,
    pub app_version: Option<String>,
    pub service_name: Option<String>,
    pub global_tags: Vec<Tag>,
    pub live_debugger_tag_cache: Option<Arc<String>>,
    pub debugger_logs_payload_sender: Arc<tokio::sync::Mutex<Option<PayloadSender>>>,
    pub debugger_diagnostics_payload_sender: Arc<tokio::sync::Mutex<Option<PayloadSender>>>,
}

impl RuntimeInfo {
    /// Shuts down the runtime.
    /// This involves shutting down all the instances in the runtime.
    pub(crate) async fn shutdown(self) {
        info!(
            "Shutting down runtime_id {} for session {}",
            self.instance_id.runtime_id, self.instance_id.session_id
        );

        debug!(
            "Successfully shut down runtime_id {} for session {}",
            self.instance_id.runtime_id, self.instance_id.session_id
        );
    }

    /// Locks the applications map and returns a mutable reference to it.
    ///
    /// # Returns
    ///
    /// * `MutexGuard<HashMap<QueueId, ActiveApplications>>` - A mutable reference to the
    ///   applications map.
    pub(crate) fn lock_applications(&self) -> MutexGuard<'_, HashMap<QueueId, ActiveApplication>> {
        self.applications.lock_or_panic()
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
    pub fn set_metadata(
        &mut self,
        env: String,
        app_version: String,
        service_name: String,
        global_tags: Vec<Tag>,
    ) {
        self.env = Some(env);
        self.app_version = Some(app_version);
        self.service_name = Some(service_name);
        self.global_tags = global_tags;
        self.live_debugger_tag_cache = None;
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
    /// * `bool` - Whether new tags were set and a new sender needs to be started.
    pub fn get_debugger_tags(
        &mut self,
        debugger_version: &dyn Display,
        runtime_id: &str,
    ) -> (Arc<String>, bool) {
        if let Some(ref cached) = self.live_debugger_tag_cache {
            return (cached.clone(), false);
        }
        if let Some(env) = &self.env {
            if let Some(version) = &self.app_version {
                let tags = Arc::new(generate_tags(
                    debugger_version,
                    env,
                    version,
                    &runtime_id,
                    &mut self.global_tags.iter(),
                ));
                self.live_debugger_tag_cache = Some(tags.clone());
                return (tags, true);
            }
        }
        let tags = Arc::new(format!("debugger_version:{debugger_version}"));
        self.live_debugger_tag_cache = Some(tags.clone());
        (tags, true)
    }
}

// TODO: APM-1079 - Add unit tests for RuntimeInfo
