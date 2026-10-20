// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::ApplicationConfig;
use crate::service::connection_session::ConnectionSession;
use crate::service::remote_configs::{RemoteConfigNotifyTarget, RemoteConfigs, RemoteConfigsGuard};
use libdd_live_debugger::sender::{DebuggerType, PayloadSender, generate_tags};
use std::fmt::Display;
use std::sync::Arc;

type DebuggerPayloadSender = Arc<tokio::sync::Mutex<Option<PayloadSender>>>;

/// The application of the request currently processed on a connection.
///
/// The remote config subscription and the live debugger senders are bound to it.
pub(crate) struct ActiveApplication {
    pub(crate) config: ApplicationConfig,
    remote_config_guard: Option<RemoteConfigsGuard>,
    live_debugger_tag_cache: Option<Arc<String>>,
    debugger_logs_payload_sender: DebuggerPayloadSender,
    debugger_snapshots_payload_sender: DebuggerPayloadSender,
    debugger_diagnostics_payload_sender: DebuggerPayloadSender,
}

impl ActiveApplication {
    pub(crate) fn new(config: ApplicationConfig) -> Self {
        ActiveApplication {
            config,
            remote_config_guard: None,
            live_debugger_tag_cache: None,
            debugger_logs_payload_sender: Default::default(),
            debugger_snapshots_payload_sender: Default::default(),
            debugger_diagnostics_payload_sender: Default::default(),
        }
    }

    pub(crate) fn has_remote_config(&self) -> bool {
        self.remote_config_guard.is_some()
    }

    pub(crate) fn update(&mut self, config: ApplicationConfig) {
        if self.config.env_name != config.env_name
            || self.config.app_version != config.app_version
            || self.config.global_tags != config.global_tags
        {
            self.live_debugger_tag_cache = None;
        }
        self.config = config;
    }

    /// (Re-)registers the application for remote config.
    ///
    /// The new registration is acquired before the old one is released, so a target shared
    /// with the previous registration is never dropped in between.
    pub(crate) fn update_remote_config(
        &mut self,
        remote_configs: &RemoteConfigs,
        session: &ConnectionSession,
        remote_config_generation: u64,
        notify_target: RemoteConfigNotifyTarget,
    ) {
        if !session.remote_config_enabled {
            return;
        }
        // Target is hashed on the sidecar side and on the PHP read side
        // (sidecar.c:ddog_remote_configs_service_env_change). PHP passes the
        // bare process_tags Vec, so we must too — otherwise SHM lookups miss.
        self.remote_config_guard = Some(remote_configs.add_runtime(
            session.remote_config_options.clone(),
            session.remote_config_interval,
            session.instance_id.clone(),
            remote_config_generation,
            notify_target,
            self.config.env_name.clone(),
            self.config.service_name.clone(),
            self.config.app_version.clone(),
            self.config.global_tags.clone(),
            self.config.dynamic_instrumentation_state,
            session.process_tags.clone(),
        ));
    }

    /// Sets the cached debugger tags if not set and returns them.
    ///
    /// # Returns
    ///
    /// * `Arc<String>` - A percent encoded string to be passed to
    ///   libdd_live_debugger::sender::send.
    /// * `bool` - Whether new tags were set and a new sender needs to be started.
    pub(crate) fn get_debugger_tags(
        &mut self,
        debugger_version: &dyn Display,
        runtime_id: &str,
    ) -> (Arc<String>, bool) {
        if let Some(ref cached) = self.live_debugger_tag_cache {
            return (cached.clone(), false);
        }
        let tags = Arc::new(generate_tags(
            debugger_version,
            &self.config.env_name,
            &self.config.app_version,
            &runtime_id,
            &mut self.config.global_tags.iter(),
        ));
        self.live_debugger_tag_cache = Some(tags.clone());
        (tags, true)
    }

    pub(crate) fn debugger_payload_sender(
        &self,
        debugger_type: DebuggerType,
    ) -> DebuggerPayloadSender {
        match debugger_type {
            DebuggerType::Diagnostics => self.debugger_diagnostics_payload_sender.clone(),
            DebuggerType::Snapshots => self.debugger_snapshots_payload_sender.clone(),
            DebuggerType::Logs => self.debugger_logs_payload_sender.clone(),
        }
    }
}
