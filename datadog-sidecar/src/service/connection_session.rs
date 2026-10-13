// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use libdd_common::{Endpoint, tag::Tag};
use libdd_dogstatsd_client::DogStatsDClient;
use libdd_live_debugger::sender::{DebuggerType, PayloadSender};
use libdd_remote_config::fetch::{ConfigInvariants, ConfigOptions};
use libdd_telemetry::config::TelemetryEndpoint;
use tracing::{debug, error, trace};

use crate::config::get_product_endpoint;
use crate::log::{MULTI_LOG_FILTER, MULTI_LOG_WRITER, MultiEnvFilterGuard, MultiWriterGuard};
use crate::service::agent_info::{AgentInfoGuard, AgentInfos};
use crate::service::application::ActiveApplication;
use crate::service::dogstatsd_pool::DogStatsDPool;
use crate::service::remote_configs::RemoteConfigNotifyTarget;
use crate::service::stats_flusher::{StatsConfig, stats_endpoint};
use crate::service::{InstanceId, RuntimeMetadata, SessionConfig};
use crate::{spawn_map_err, tracer};

/// Session-level state of one sidecar connection.
///
/// A connection serves one PHP thread, which processes one request at a time, so this is all the
/// state its messages need. It is replaced as a whole when updated, so tasks spawned by a
/// handler keep a consistent snapshot.
#[derive(Clone)]
pub struct ConnectionSession {
    pub instance_id: InstanceId,
    pub telemetry_config: libdd_telemetry::config::Config,
    pub tracer_config: tracer::Config,
    debugger_config: libdd_live_debugger::sender::Config,
    pub dogstatsd: Option<Arc<DogStatsDClient>>,
    pub remote_config_options: ConfigOptions,
    pub remote_config_interval: Duration,
    pub remote_config_enabled: bool,
    pub(crate) stats_config: Option<StatsConfig>,
    pub otlp_metrics_endpoint: Option<Endpoint>,
    pub process_tags: Vec<Tag>,
    auto_resolved_service_name: Option<String>,
    user_service_defined: bool,
    #[cfg(unix)]
    peer_pid: u32,
    #[cfg(windows)]
    remote_config_notify_target: Option<RemoteConfigNotifyTarget>,
    agent_info: Option<Arc<AgentInfoGuard>>,
    _log_guard: Option<Arc<(MultiEnvFilterGuard<'static>, MultiWriterGuard<'static>)>>,
}

/// Shared handle on the session state of one connection.
///
/// In-process consumers (like the AppSec helper) keep it to resolve telemetry for their
/// connection; it observes later reconfigurations and stays usable after the connection closed.
/// Note (TODO): This is currently needlessly complicated, because appsec has still a separate
/// lifecycle. Once their lifecycle aligns perfectly (because of merged extensions), we can
/// simplify this and drop the ArcSwapOption. The primary pain point is the telemetry.
#[derive(Clone, Default)]
pub struct ConnectionSessionHandle(Arc<ArcSwapOption<ConnectionSession>>);

impl std::fmt::Debug for ConnectionSessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.load() {
            Some(session) => session.instance_id.fmt(f),
            None => f.write_str("<unconfigured connection>"),
        }
    }
}

impl ConnectionSessionHandle {
    pub fn load(&self) -> Option<Arc<ConnectionSession>> {
        self.0.load_full()
    }

    pub(crate) fn store(&self, session: ConnectionSession) {
        self.0.store(Some(Arc::new(session)));
    }

    /// Copy-on-write update. Only the connection itself writes, so there is no lost update.
    pub(crate) fn update(&self, f: impl FnOnce(&mut ConnectionSession)) -> bool {
        let Some(current) = self.0.load_full() else {
            return false;
        };
        let mut session = (*current).clone();
        f(&mut session);
        self.store(session);
        true
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl ConnectionSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_config(
        instance_id: InstanceId,
        config: SessionConfig,
        #[cfg(unix)] peer_pid: u32,
        #[cfg(windows)] remote_config_notify_target: Option<RemoteConfigNotifyTarget>,
        agent_infos: &AgentInfos,
        dogstatsd: &DogStatsDPool,
    ) -> ConnectionSession {
        let mut telemetry_config = libdd_telemetry::config::Config::from_env();
        let endpoint = get_product_endpoint(
            libdd_telemetry::config::PROD_INTAKE_SUBDOMAIN,
            &config.endpoint,
        );
        telemetry_config
            .set_endpoint(TelemetryEndpoint {
                api_key: endpoint.api_key.as_deref().map(str::to_owned),
                test_token: endpoint.test_token.as_deref().map(str::to_owned),
                timeout_ms: endpoint.timeout_ms,
                use_system_resolver: endpoint.use_system_resolver,
                ..Default::default()
            })
            .ok();
        telemetry_config.set_endpoint_uri(endpoint.url).ok();
        telemetry_config.telemetry_heartbeat_interval = config.telemetry_heartbeat_interval;
        telemetry_config.telemetry_extended_heartbeat_interval =
            config.telemetry_extended_heartbeat_interval;
        telemetry_config.session_id = Some(instance_id.session_id.clone());
        telemetry_config.parent_session_id = config.parent_session_id;
        telemetry_config.root_session_id = config.root_session_id;

        let mut tracer_config = tracer::Config::default();
        tracer_config
            .set_endpoint(get_product_endpoint(
                libdd_trace_utils::config_utils::PROD_INTAKE_SUBDOMAIN,
                &config.endpoint,
            ))
            .ok();
        tracer_config.language.clone_from(&config.language);
        tracer_config
            .language_version
            .clone_from(&config.language_version);
        tracer_config
            .tracer_version
            .clone_from(&config.tracer_version);
        tracer_config.retry_interval =
            u64::try_from(config.retry_interval.as_millis()).unwrap_or(u64::MAX);

        let mut debugger_config = libdd_live_debugger::sender::Config::default();
        debugger_config
            .set_endpoint(get_product_endpoint(
                libdd_live_debugger::sender::PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN,
                &config.endpoint,
            ))
            .ok();

        // no agent info if agentless
        let agent_info = config
            .endpoint
            .api_key
            .is_none()
            .then(|| Arc::new(agent_infos.query_for(config.endpoint.clone())));

        let stats_config = Some(StatsConfig {
            endpoint: stats_endpoint(&config.endpoint).unwrap_or_else(|| config.endpoint.clone()),
            flush_interval: config.flush_interval,
            hostname: if config.hostname.is_empty() {
                sys_info::hostname().unwrap_or_default()
            } else {
                config.hostname.clone()
            },
            process_tags: String::new(),
            root_service: config.root_service.clone(),
            language: config.language.clone(),
            tracer_version: config.tracer_version.clone(),
        });

        let mut session = ConnectionSession {
            instance_id,
            telemetry_config,
            tracer_config,
            debugger_config,
            dogstatsd: dogstatsd.client_for(&config.dogstatsd_endpoint),
            remote_config_options: ConfigOptions {
                invariants: ConfigInvariants {
                    language: config.language,
                    tracer_version: config.tracer_version,
                    endpoint: config.endpoint,
                    agentless: None,
                },
                products: config.remote_config_products,
                capabilities: config.remote_config_capabilities,
            },
            remote_config_interval: config.remote_config_poll_interval,
            remote_config_enabled: config.remote_config_enabled,
            stats_config,
            otlp_metrics_endpoint: config.otlp_metrics_endpoint,
            process_tags: config.process_tags,
            auto_resolved_service_name: config.default_service_name,
            user_service_defined: false,
            #[cfg(unix)]
            peer_pid,
            #[cfg(windows)]
            remote_config_notify_target,
            agent_info,
            _log_guard: Some(Arc::new((
                MULTI_LOG_FILTER.add(config.log_level),
                MULTI_LOG_WRITER.add(config.log_file),
            ))),
        };
        session.refresh_stats_process_tags();
        session
    }

    /// A session without any endpoints, for tests to fill in what they need.
    #[cfg(test)]
    pub(crate) fn for_test(instance_id: InstanceId) -> ConnectionSession {
        ConnectionSession {
            instance_id,
            telemetry_config: Default::default(),
            tracer_config: Default::default(),
            debugger_config: Default::default(),
            dogstatsd: None,
            remote_config_options: ConfigOptions {
                invariants: ConfigInvariants {
                    language: String::new(),
                    tracer_version: String::new(),
                    endpoint: Endpoint::default(),
                    agentless: None,
                },
                products: vec![],
                capabilities: vec![],
            },
            remote_config_interval: Duration::from_secs(5),
            remote_config_enabled: false,
            stats_config: None,
            otlp_metrics_endpoint: None,
            process_tags: vec![],
            auto_resolved_service_name: None,
            user_service_defined: false,
            #[cfg(unix)]
            peer_pid: 0,
            #[cfg(windows)]
            remote_config_notify_target: None,
            agent_info: None,
            _log_guard: None,
        }
    }

    pub fn runtime_metadata(&self) -> RuntimeMetadata {
        RuntimeMetadata::new(
            self.tracer_config.language.clone(),
            self.tracer_config.language_version.clone(),
            self.tracer_config.tracer_version.clone(),
        )
    }

    pub fn process_tags_with_svc_source(&self) -> Vec<Tag> {
        let mut tags = self.process_tags.clone();
        if self.user_service_defined {
            if let Ok(tag) = Tag::new("svc.user", "true") {
                tags.push(tag);
            }
        } else if let Some(name) = self.auto_resolved_service_name.as_ref() {
            if let Ok(tag) = Tag::new("svc.auto", name.clone()) {
                tags.push(tag);
            }
        }
        tags
    }

    fn refresh_stats_process_tags(&mut self) {
        let process_tags = self
            .process_tags_with_svc_source()
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if let Some(stats) = self.stats_config.as_mut() {
            stats.process_tags = process_tags;
        }
    }

    pub(crate) fn set_process_tags(&mut self, process_tags: Vec<Tag>) {
        self.process_tags = process_tags;
        self.refresh_stats_process_tags();
    }

    pub(crate) fn set_user_service_defined(&mut self, is_defined: bool) {
        self.user_service_defined = is_defined;
        self.refresh_stats_process_tags();
    }

    pub(crate) fn set_test_session_token(&mut self, token: Option<Cow<'static, str>>) {
        self.telemetry_config.set_endpoint_test_token(token.clone());
        self.tracer_config.set_endpoint_test_token(token.clone());
        if let Some(endpoint) = self.otlp_metrics_endpoint.as_mut() {
            endpoint.test_token = token.clone();
        }
        // So that newly created concentrators carry the test token.
        if let Some(stats) = self.stats_config.as_mut() {
            stats.endpoint.test_token = token;
        }
        // TODO(APMSP-1377): the dogstatsd-client doesn't support test_session tokens yet
    }

    /// Where remote config updates for this connection's applications are signalled.
    pub(crate) fn notify_target(&self) -> Option<RemoteConfigNotifyTarget> {
        #[cfg(unix)]
        {
            // A peer outside our pid namespace has no usable pid; kill(0, ..) would signal our
            // own process group.
            let pid = libc::pid_t::try_from(self.peer_pid).ok()?;
            (pid > 0).then_some(RemoteConfigNotifyTarget { pid })
        }
        #[cfg(windows)]
        {
            self.remote_config_notify_target.clone()
        }
    }

    fn debugger_config(&self) -> Cow<'_, libdd_live_debugger::sender::Config> {
        match &self.agent_info {
            Some(agent_info) if agent_info.lacks_debugger_v2_endpoint() => {
                let mut config = self.debugger_config.clone();
                config.downgrade_to_diagnostics_endpoint();
                Cow::Owned(config)
            }
            _ => Cow::Borrowed(&self.debugger_config),
        }
    }

    pub(crate) fn send_debugger_data<R: AsRef<[u8]> + Sync + Send + 'static>(
        self: &Arc<Self>,
        app: &mut ActiveApplication,
        debugger_type: DebuggerType,
        payload: R,
    ) {
        async fn do_send(
            session: Arc<ConnectionSession>,
            debugger_type: DebuggerType,
            new_tags: bool,
            tags: Arc<String>,
            guard: Arc<tokio::sync::Mutex<Option<PayloadSender>>>,
            payload: &[u8],
        ) -> anyhow::Result<()> {
            async fn finish_sender(debugger_type: DebuggerType, sender: PayloadSender) {
                match sender.finish().await {
                    Ok(payloads) => debug!(
                        "Successfully sent {payloads} payloads to live debugger {debugger_type:?} endpoint"
                    ),
                    Err(e) => error!("Error sending to live debugger endpoint: {e:?}"),
                }
            }

            let mut sender = guard.lock().await;
            if new_tags {
                if let Some(sender) = sender.take() {
                    spawn_map_err!(finish_sender(debugger_type, sender), |e| {
                        error!("Error sending to live debugger {debugger_type:?} endpoint: {e:?}");
                    });
                }
            }
            if sender.is_none() {
                *sender = Some(PayloadSender::new(
                    &session.debugger_config(),
                    debugger_type,
                    tags.as_str(),
                )?);
                let guard = guard.clone();
                spawn_map_err!(
                    async move {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if let Some(sender) = guard.lock().await.take() {
                            finish_sender(debugger_type, sender).await;
                        }
                    },
                    |e| error!("Error sending to live debugger {debugger_type:?} endpoint: {e:?}")
                );
            }
            trace!(
                "Submitting live debugger {debugger_type:?} payload {:?}",
                String::from_utf8_lossy(payload)
            );

            #[allow(clippy::unwrap_used)]
            sender.as_mut().unwrap().append(payload).await
        }

        async fn send<R: AsRef<[u8]> + Sync + Send>(
            session: Arc<ConnectionSession>,
            debugger_type: DebuggerType,
            new_tags: bool,
            tags: Arc<String>,
            guard: Arc<tokio::sync::Mutex<Option<PayloadSender>>>,
            payload: R,
        ) {
            let payload = payload.as_ref();
            if let Err(e) = do_send(session, debugger_type, new_tags, tags, guard, payload).await {
                error!("Error sending to live debugger {debugger_type:?} endpoint: {e:?}");
                debug!("Attempted to send the following payload: {:?}", payload);
            }
        }

        let (tags, new_tags) = app.get_debugger_tags(
            &self
                .remote_config_options
                .invariants
                .tracer_version
                .as_str(),
            &self.instance_id.runtime_id,
        );
        let sender = app.debugger_payload_sender(debugger_type);
        let session = self.clone();
        spawn_map_err!(
            send(session, debugger_type, new_tags, tags, sender, payload),
            |e| {
                error!("Error sending to live debugger {debugger_type:?} endpoint: {e:?}");
            }
        );
    }
}
