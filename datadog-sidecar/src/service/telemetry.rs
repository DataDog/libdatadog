// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{InstanceId, RuntimeMetadata, SidecarAction, SidecarServer};
use anyhow::{anyhow, Result};
use libdd_common::MutexExt;
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::primary_sidecar_identifier;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::one_way_shared_memory::OneWayShmWriter;
use datadog_ipc::platform::NamedShmHandle;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use zwohash::ZwoHasher;

use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::tag::Tag;
use libdd_telemetry::worker::TelemetryWorkerBuilder;
use serde::{Deserialize, Serialize};
use std::ops::Sub;
use std::sync::LazyLock;
use std::time::SystemTime;

use libdd_telemetry::config::Config;
use libdd_telemetry::data::{self, Integration};
use libdd_telemetry::metrics::{ContextKey, MetricContext};
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerFlavor};

/// Sidecar's telemetry worker is native-only, so its handle is pinned to
/// [`NativeCapabilities`].
type TelemetryWorkerHandle = libdd_telemetry::worker::TelemetryWorkerHandle<NativeCapabilities>;
use manual_future::ManualFuture;
use serde_with::{serde_as, VecSkipError};
use tokio::time::{sleep, sleep_until, Instant as TokioInstant};

#[derive(Debug)]
pub struct InternalTelemetryActions {
    pub instance_id: InstanceId,
    pub service_name: String,
    pub env_name: String,
    pub actions: Vec<InternalTelemetryAction>,
}

#[derive(Debug)]
pub enum InternalTelemetryAction {
    TelemetryAction(TelemetryActions),
    RegisterTelemetryMetric(MetricContext),
    AddMetricPoint((f64, String, Vec<Tag>)),
}

#[derive(Clone, Debug)]
pub(crate) enum DirectTelemetryRetirement {
    Runtimes(HashSet<InstanceId>),
    Session(String),
}

pub(crate) enum DirectTelemetryMessage {
    Actions(InternalTelemetryActions),
    Retire {
        scope: DirectTelemetryRetirement,
        acknowledgement: oneshot::Sender<()>,
    },
    #[cfg(test)]
    Barrier(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct TelemetryActionSender {
    sender: mpsc::Sender<DirectTelemetryMessage>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TelemetryActionSendError {
    Full,
    Closed,
}

impl std::fmt::Display for TelemetryActionSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => formatter.write_str("direct telemetry channel is full"),
            Self::Closed => formatter.write_str("direct telemetry channel is closed"),
        }
    }
}

impl std::error::Error for TelemetryActionSendError {}

impl TelemetryActionSender {
    pub fn try_send(
        &self,
        actions: InternalTelemetryActions,
    ) -> std::result::Result<(), TelemetryActionSendError> {
        self.sender
            .try_send(DirectTelemetryMessage::Actions(actions))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TelemetryActionSendError::Full,
                mpsc::error::TrySendError::Closed(_) => TelemetryActionSendError::Closed,
            })
    }

    #[cfg(test)]
    pub(crate) async fn send_actions(
        &self,
        actions: InternalTelemetryActions,
    ) -> std::result::Result<(), mpsc::error::SendError<InternalTelemetryActions>> {
        self.sender
            .send(DirectTelemetryMessage::Actions(actions))
            .await
            .map_err(|error| match error.0 {
                DirectTelemetryMessage::Actions(actions) => mpsc::error::SendError(actions),
                _ => unreachable!("send_actions only submits direct telemetry action messages"),
            })
    }

    pub(crate) async fn retire(&self, scope: DirectTelemetryRetirement) -> Result<()> {
        let (acknowledgement, acknowledged) = oneshot::channel();
        self.sender
            .send(DirectTelemetryMessage::Retire {
                scope,
                acknowledgement,
            })
            .await
            .map_err(|_| anyhow!("direct telemetry receiver is unavailable"))?;
        acknowledged
            .await
            .map_err(|_| anyhow!("direct telemetry cleanup acknowledgement was dropped"))
    }

    #[cfg(test)]
    pub(crate) async fn barrier(&self) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(DirectTelemetryMessage::Barrier(sender))
            .await
            .map_err(|_| anyhow!("direct telemetry receiver is unavailable"))?;
        receiver
            .await
            .map_err(|_| anyhow!("direct telemetry receiver barrier was dropped"))
    }
}

#[cfg(test)]
pub(crate) fn direct_telemetry_channel() -> (
    TelemetryActionSender,
    mpsc::Receiver<DirectTelemetryMessage>,
) {
    let (sender, receiver) = mpsc::channel(1000);
    (TelemetryActionSender { sender }, receiver)
}

pub(crate) async fn telemetry_action_receiver_task(
    sidecar: SidecarServer,
    mut rx: mpsc::Receiver<DirectTelemetryMessage>,
) {
    info!("Starting telemetry action receiver task...");
    let mut pending: Vec<PerClientTelemetryBatch> = Vec::new();

    while let Some(entry) = next_entry(&mut pending, &mut rx).await {
        let ReceiverEntry::Batch(batch) = entry else {
            match entry {
                ReceiverEntry::Retire {
                    scope,
                    acknowledgement,
                } => {
                    match &scope {
                        DirectTelemetryRetirement::Runtimes(instances) => {
                            pending.retain(|batch| !instances.contains(&batch.key.0));
                            sidecar.metrics_logs_clients.remove_runtimes(instances);
                        }
                        DirectTelemetryRetirement::Session(session_id) => {
                            pending.retain(|batch| batch.key.0.session_id != *session_id);
                            sidecar.metrics_logs_clients.remove_session(session_id);
                        }
                    }
                    let _ = acknowledgement.send(());
                }
                #[cfg(test)]
                ReceiverEntry::Barrier(acknowledgement) => {
                    let _ = acknowledgement.send(());
                }
                ReceiverEntry::Batch(_) => unreachable!(),
            }
            continue;
        };
        let Some(_telemetry_client) = batch.get_client(&sidecar) else {
            batch.defer_or_drop(&mut pending);
            continue;
        };

        batch.deliver(&sidecar).await;
    }

    let total_pending: usize = pending.iter().map(|s| s.actions.len()).sum();
    if total_pending > 0 {
        warn!(
            "Telemetry action receiver task shutting down with {total_pending} undelivered \
             pending batches",
        );
    }
    info!("Telemetry action receiver task shutting down.");
}

async fn next_entry(
    pending: &mut Vec<PerClientTelemetryBatch>,
    rx: &mut mpsc::Receiver<DirectTelemetryMessage>,
) -> Option<ReceiverEntry> {
    loop {
        if pending.is_empty() {
            return rx.recv().await.map(ReceiverEntry::from);
        }

        // we have batches to retry

        #[allow(clippy::unwrap_used)]
        let min_pos = pending
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.next_attempt_at)
            .map(|(i, _)| i)
            .unwrap();
        let deadline = pending[min_pos].next_attempt_at;

        tokio::select! {
            biased;
            _ = sleep_until(deadline) => {
                return Some(ReceiverEntry::Batch(TelemetryBatch::Deferred(
                    pending.swap_remove(min_pos),
                )));
            }
            result = rx.recv() => match result {
                Some(DirectTelemetryMessage::Actions(batch)) => {
                    let key = (
                        &batch.instance_id,
                        batch.service_name.as_str(),
                        batch.env_name.as_str(),
                    );
                    if let Some(deferred) = pending.iter_mut().find(|batch| {
                        batch.key.0 == *key.0
                            && batch.key.1 == key.1
                            && batch.key.2 == key.2
                    }) {
                        deferred.actions.push_back(batch);
                    } else {
                        return Some(ReceiverEntry::Batch(TelemetryBatch::Fresh(batch)));
                    }
                }
                Some(message) => return Some(ReceiverEntry::from(message)),
                None => return None,
            },
        }
    }
}

enum ReceiverEntry {
    Batch(TelemetryBatch),
    Retire {
        scope: DirectTelemetryRetirement,
        acknowledgement: oneshot::Sender<()>,
    },
    #[cfg(test)]
    Barrier(oneshot::Sender<()>),
}

impl From<DirectTelemetryMessage> for ReceiverEntry {
    fn from(message: DirectTelemetryMessage) -> Self {
        match message {
            DirectTelemetryMessage::Actions(actions) => Self::Batch(TelemetryBatch::Fresh(actions)),
            DirectTelemetryMessage::Retire {
                scope,
                acknowledgement,
            } => Self::Retire {
                scope,
                acknowledgement,
            },
            #[cfg(test)]
            DirectTelemetryMessage::Barrier(acknowledgement) => Self::Barrier(acknowledgement),
        }
    }
}

async fn deliver_batch(
    actions: Vec<InternalTelemetryAction>,
    sidecar: &SidecarServer,
    instance_id: &InstanceId,
    service: &str,
    env: &str,
) {
    for it_action in actions {
        match it_action {
            InternalTelemetryAction::TelemetryAction(action) => {
                let Some((_, client)) =
                    get_active_direct_worker(sidecar, instance_id, service, env)
                else {
                    warn!("Telemetry client unavailable during delivery for {service}/{env}");
                    continue;
                };
                let action_str = format!("{action:?}");
                match client.send_msg(action).await {
                    Ok(_) => debug!("Sent telemetry action to TelemetryWorker: {action_str}"),
                    Err(e) => warn!(
                        "Failed to send telemetry action {action_str} to TelemetryWorker: {e}"
                    ),
                }
            }
            InternalTelemetryAction::RegisterTelemetryMetric(metric) => {
                let metric_name = metric.name.clone();
                match sidecar.metrics_logs_clients.register_metric_with_outcome(
                    instance_id,
                    service,
                    env,
                    metric,
                ) {
                    MetricRegistrationOutcome::RejectedCapacity { limit } => warn!(
                        "Rejected telemetry metric registration: session={} service={} env={} \
                         metric={} capacity={limit}",
                        instance_id.session_id, service, env, metric_name
                    ),
                    outcome => debug!(
                        "Registered telemetry metric: session={} service={} env={} metric={} \
                         outcome={outcome:?}",
                        instance_id.session_id, service, env, metric_name
                    ),
                }
            }
            InternalTelemetryAction::AddMetricPoint((value, name, tags)) => {
                let metric_name = name.clone();
                let Some((telemetry_client, client)) =
                    get_active_direct_worker(sidecar, instance_id, service, env)
                else {
                    warn!(
                        "Telemetry client unavailable for metric point {metric_name} in \
                         {service}/{env}"
                    );
                    continue;
                };
                let point = telemetry_client
                    .lock_or_panic()
                    .as_ref()
                    .and_then(|t| t.to_telemetry_point((name, value, tags)));
                match point {
                    Some(p) => {
                        if let Err(e) = client.send_msg(p).await {
                            warn!("Failed to send telemetry point to TelemetryWorker: {e}");
                        }
                    }
                    None => warn!(
                        "Attempted to send telemetry point for unregistered metric: {metric_name}"
                    ),
                }
            }
        }
    }
}

fn get_active_direct_worker(
    sidecar: &SidecarServer,
    instance_id: &InstanceId,
    service: &str,
    env: &str,
) -> Option<(
    Arc<Mutex<Option<TelemetryCachedClient>>>,
    TelemetryWorkerHandle,
)> {
    let telemetry_client = get_telemetry_client(sidecar, instance_id, service, env)?;
    let worker = telemetry_client
        .lock_or_panic()
        .as_ref()
        .filter(|client| !client.is_stopping())
        .map(|client| client.worker.clone())?;
    Some((telemetry_client, worker))
}

enum TelemetryBatch {
    Fresh(InternalTelemetryActions),
    Deferred(PerClientTelemetryBatch),
}

impl TelemetryBatch {
    fn get_client(
        &self,
        sidecar: &SidecarServer,
    ) -> Option<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        match self {
            TelemetryBatch::Fresh(a) => {
                get_telemetry_client(sidecar, &a.instance_id, &a.service_name, &a.env_name)
            }
            TelemetryBatch::Deferred(d) => {
                get_telemetry_client(sidecar, &d.key.0, &d.key.1, &d.key.2)
            }
        }
    }

    const RETRY_DELAY: Duration = Duration::from_millis(1500);
    const MAX_ATTEMPTS: u8 = 3;

    fn defer_or_drop(self, pending: &mut Vec<PerClientTelemetryBatch>) {
        match self {
            TelemetryBatch::Fresh(actions) => {
                info!(
                    "Telemetry client not ready for {}/{}, \
                     retrying in {}ms ({} left)",
                    actions.service_name,
                    actions.env_name,
                    Self::RETRY_DELAY.as_millis(),
                    Self::MAX_ATTEMPTS - 1,
                );
                let next_at = TokioInstant::now() + Self::RETRY_DELAY;
                pending.push(PerClientTelemetryBatch {
                    key: (
                        actions.instance_id.clone(),
                        actions.service_name.clone(),
                        actions.env_name.clone(),
                    ),
                    actions: VecDeque::from([actions]),
                    attempts_left: Self::MAX_ATTEMPTS - 1,
                    next_attempt_at: next_at,
                });
            }
            TelemetryBatch::Deferred(deferred) => {
                debug_assert!(!deferred.actions.is_empty());
                let (_, service_name, env_name) = &deferred.key;
                let remaining = deferred.attempts_left - 1;
                if remaining > 0 {
                    info!(
                        "Telemetry client not ready for {service_name}/{env_name}, \
                         retrying in {}ms ({remaining} left)",
                        Self::RETRY_DELAY.as_millis(),
                    );
                    pending.push(PerClientTelemetryBatch {
                        key: deferred.key,
                        actions: deferred.actions,
                        attempts_left: remaining,
                        next_attempt_at: TokioInstant::now() + Self::RETRY_DELAY,
                    });
                } else {
                    let count: usize = deferred.actions.iter().map(|b| b.actions.len()).sum();
                    warn!(
                        "Dropping {count} telemetry actions for {service_name}/{env_name}: \
                         telemetry client never became ready after {} attempts",
                        Self::MAX_ATTEMPTS,
                    );
                }
            }
        }
    }

    async fn deliver(self, sidecar: &SidecarServer) {
        match self {
            TelemetryBatch::Fresh(actions) => {
                deliver_batch(
                    actions.actions,
                    sidecar,
                    &actions.instance_id,
                    &actions.service_name,
                    &actions.env_name,
                )
                .await;
            }
            TelemetryBatch::Deferred(deferred) => {
                debug_assert!(!deferred.actions.is_empty());
                for batch in deferred.actions {
                    deliver_batch(
                        batch.actions,
                        sidecar,
                        &batch.instance_id,
                        &batch.service_name,
                        &batch.env_name,
                    )
                    .await;
                }
            }
        }
    }
}

struct PerClientTelemetryBatch {
    key: (InstanceId, ServiceString, EnvString),
    actions: VecDeque<InternalTelemetryActions>, // invariant: non-empty
    attempts_left: u8,
    next_attempt_at: TokioInstant,
}

type ComposerCache = HashMap<PathBuf, (SystemTime, Arc<Vec<data::Dependency>>)>;

static COMPOSER_CACHE: LazyLock<tokio::sync::Mutex<ComposerCache>> =
    LazyLock::new(|| tokio::sync::Mutex::new(Default::default()));

static LAST_CACHE_CLEAN: AtomicU64 = AtomicU64::new(0);

static TELEMETRY_ACTION_SENDER: OnceLock<TelemetryActionSender> = OnceLock::new();

#[serde_as]
#[derive(Deserialize)]
struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    packages: Vec<data::Dependency>,
}

pub struct TelemetryCachedEntry {
    last_used: Instant,
    pub client: Arc<Mutex<Option<TelemetryCachedClient>>>,
}

#[derive(Default)]
pub struct InitialTelemetryData {
    configurations: Vec<data::Configuration>,
    dependencies: Vec<data::Dependency>,
    integrations: Vec<data::Integration>,
}

impl InitialTelemetryData {
    pub fn from_actions(actions: &[SidecarAction]) -> Self {
        Self::from_action_refs(actions.iter())
    }

    fn from_pending_actions(actions: &[PendingApplicationAction]) -> Self {
        Self::from_action_refs(actions.iter().map(|pending_action| &pending_action.action))
    }

    fn from_action_refs<'a>(actions: impl Iterator<Item = &'a SidecarAction>) -> Self {
        let mut initial = Self::default();
        for action in actions {
            match action {
                SidecarAction::Telemetry(TelemetryActions::AddConfig(value)) => {
                    initial.configurations.push(value.clone());
                }
                SidecarAction::Telemetry(TelemetryActions::AddDependency(value)) => {
                    initial.dependencies.push(value.clone());
                }
                SidecarAction::Telemetry(TelemetryActions::AddIntegration(value)) => {
                    initial.integrations.push(value.clone());
                }
                _ => {}
            }
        }
        initial
    }

    pub(crate) fn contains_seeded_action(action: &SidecarAction) -> bool {
        matches!(
            action,
            SidecarAction::Telemetry(
                TelemetryActions::AddConfig(_)
                    | TelemetryActions::AddDependency(_)
                    | TelemetryActions::AddIntegration(_)
            )
        )
    }
}

struct PendingTelemetryActions {
    last_used: Instant,
    actions: Vec<PendingApplicationAction>,
}

#[derive(Debug)]
pub(crate) struct PendingApplicationAction {
    pub(crate) origin: InstanceId,
    pub(crate) action: SidecarAction,
    pub(crate) metric_registration: Option<MetricContext>,
}

impl PendingApplicationAction {
    pub(crate) fn from_actions(
        origin: &InstanceId,
        actions: Vec<SidecarAction>,
        metric_registrations: &HashMap<String, MetricContext>,
    ) -> Vec<Self> {
        actions
            .into_iter()
            .map(|action| {
                let metric_registration = match &action {
                    SidecarAction::AddTelemetryMetricPoint((name, _, _)) => {
                        metric_registrations.get(name).cloned()
                    }
                    _ => None,
                };
                Self {
                    origin: origin.clone(),
                    action,
                    metric_registration,
                }
            })
            .collect()
    }
}

pub(crate) enum ApplicationTelemetryDispatch {
    Pending,
    Ready {
        client: Arc<Mutex<Option<TelemetryCachedClient>>>,
        actions: Vec<PendingApplicationAction>,
        created: bool,
        remove_client: bool,
    },
}

enum ApplicationShmState {
    NotRequired,
    Ready(OneWayShmWriter<NamedShmHandle>),
    RetryAt { path: CString, deadline: Instant },
}

pub struct TelemetryCachedClient {
    pub worker: TelemetryWorkerHandle,
    shm_state: ApplicationShmState,
    pub telemetry_metrics: HashMap<String, ContextKey>,
    pub handle: Option<JoinHandle<()>>,
    pub shared: TelemetryCachedClientShmData,
    stopping: bool,
}

pub(crate) struct TelemetryWorkerMetadata<'a> {
    service: &'a str,
    env: &'a str,
    instance_id: &'a InstanceId,
    runtime_meta: &'a RuntimeMetadata,
    process_tags: Vec<Tag>,
}

impl<'a> TelemetryWorkerMetadata<'a> {
    pub(crate) fn new(
        service: &'a str,
        env: &'a str,
        instance_id: &'a InstanceId,
        runtime_meta: &'a RuntimeMetadata,
        process_tags: Vec<Tag>,
    ) -> Self {
        Self {
            service,
            env,
            instance_id,
            runtime_meta,
            process_tags,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct TelemetryCachedClientShmData {
    pub config_sent: bool,
    pub integrations: HashSet<Integration>,
    pub composer_paths: HashSet<PathBuf>,
    pub last_endpoints_push: SystemTime,
}

impl Default for TelemetryCachedClientShmData {
    fn default() -> Self {
        TelemetryCachedClientShmData {
            config_sent: false,
            integrations: HashSet::new(),
            composer_paths: HashSet::new(),
            last_endpoints_push: SystemTime::UNIX_EPOCH,
        }
    }
}

impl TelemetryCachedClient {
    fn worker_builder(metadata: &TelemetryWorkerMetadata<'_>) -> TelemetryWorkerBuilder {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            metadata.service.to_string(),
            metadata.runtime_meta.language_name.to_string(),
            metadata.runtime_meta.language_version.to_string(),
            metadata.runtime_meta.tracer_version.to_string(),
        );

        builder.runtime_id = Some(metadata.instance_id.runtime_id.clone());

        builder.application.env = Some(metadata.env.to_string());
        builder.application.process_tags = (!metadata.process_tags.is_empty()).then(|| {
            metadata
                .process_tags
                .iter()
                .map(|tag| tag.to_string())
                .collect::<Vec<_>>()
                .join(",")
        });
        builder
    }

    fn new(
        metadata: TelemetryWorkerMetadata<'_>,
        get_config: impl FnOnce() -> Config,
        initial: InitialTelemetryData,
    ) -> Result<Self> {
        Self::new_with_shm_factory_at(metadata, get_config, initial, Instant::now(), |path| {
            OneWayShmWriter::<NamedShmHandle>::new(path.clone())
        })
    }

    fn new_with_shm_factory_at(
        metadata: TelemetryWorkerMetadata<'_>,
        get_config: impl FnOnce() -> Config,
        initial: InitialTelemetryData,
        now: Instant,
        create: impl FnOnce(&CString) -> std::io::Result<OneWayShmWriter<NamedShmHandle>>,
    ) -> Result<Self> {
        let mut builder = Self::worker_builder(&metadata);
        builder.config = get_config();
        builder.configurations.extend(initial.configurations);
        builder.dependencies.extend(initial.dependencies);
        builder.integrations.extend(initial.integrations);

        let (handle, _join) = builder.spawn();
        info!("spawned telemetry worker");
        handle.send_start()?;

        let path = path_for_telemetry(metadata.service, metadata.env);
        let shm_state = match create(&path) {
            Ok(writer) => ApplicationShmState::Ready(writer),
            Err(error) => {
                warn!("Failed to create telemetry shared-memory writer: {error:?}");
                ApplicationShmState::RetryAt {
                    path,
                    deadline: now + Duration::from_secs(60),
                }
            }
        };

        Ok(Self {
            worker: handle,
            shm_state,
            shared: TelemetryCachedClientShmData::default(),
            telemetry_metrics: Default::default(),
            handle: None,
            stopping: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_with_shm_factory(
        metadata: TelemetryWorkerMetadata<'_>,
        get_config: impl FnOnce() -> Config,
        initial: InitialTelemetryData,
        now: Instant,
        create: impl FnOnce(&CString) -> std::io::Result<OneWayShmWriter<NamedShmHandle>>,
    ) -> Result<Self> {
        Self::new_with_shm_factory_at(metadata, get_config, initial, now, create)
    }

    pub(crate) fn spawn_metrics_logs_worker(
        service: &str,
        env: &str,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMetadata,
        get_config: impl FnOnce() -> Config,
        process_tags: Vec<Tag>,
    ) -> TelemetryWorkerHandle {
        let metadata =
            TelemetryWorkerMetadata::new(service, env, instance_id, runtime_meta, process_tags);
        let mut builder = Self::worker_builder(&metadata);
        builder.config = get_config();
        builder.flavor = TelemetryWorkerFlavor::MetricsLogs;

        let (handle, _join) = builder.spawn();
        info!("spawned metrics/logs telemetry worker");
        handle.send_start().ok();
        handle
    }

    fn new_metrics_logs(
        service: &str,
        env: &str,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMetadata,
        get_config: impl FnOnce() -> Config,
        process_tags: Vec<Tag>,
    ) -> Self {
        Self {
            worker: Self::spawn_metrics_logs_worker(
                service,
                env,
                instance_id,
                runtime_meta,
                get_config,
                process_tags,
            ),
            shm_state: ApplicationShmState::NotRequired,
            telemetry_metrics: HashMap::new(),
            handle: None,
            shared: TelemetryCachedClientShmData::default(),
            stopping: false,
        }
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping
    }

    pub(crate) fn mark_stopping(&mut self) {
        if let ApplicationShmState::Ready(shm_writer) =
            std::mem::replace(&mut self.shm_state, ApplicationShmState::NotRequired)
        {
            shm_writer.write(&[]);
        }
        self.stopping = true;
    }

    pub fn write_shm_file(&mut self) {
        self.write_shm_file_at(Instant::now(), |path| {
            OneWayShmWriter::<NamedShmHandle>::new(path.clone())
        });
    }

    pub(crate) fn retry_shm_file_if_due(&mut self) {
        let now = Instant::now();
        if matches!(
            &self.shm_state,
            ApplicationShmState::RetryAt { deadline, .. } if now >= *deadline
        ) {
            self.write_shm_file_at(now, |path| {
                OneWayShmWriter::<NamedShmHandle>::new(path.clone())
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn has_ready_shm(&self) -> bool {
        matches!(self.shm_state, ApplicationShmState::Ready(_))
    }

    fn write_shm_file_at(
        &mut self,
        now: Instant,
        create: impl FnOnce(&CString) -> std::io::Result<OneWayShmWriter<NamedShmHandle>>,
    ) {
        let serialized = match bincode::serialize(&self.shared) {
            Ok(value) => value,
            Err(error) => {
                warn!("Failed to serialize telemetry data for shared memory: {error}");
                return;
            }
        };

        if matches!(
            &self.shm_state,
            ApplicationShmState::RetryAt { deadline, .. } if now >= *deadline
        ) {
            let ApplicationShmState::RetryAt { path, .. } =
                std::mem::replace(&mut self.shm_state, ApplicationShmState::NotRequired)
            else {
                unreachable!();
            };
            self.shm_state = match create(&path) {
                Ok(writer) => ApplicationShmState::Ready(writer),
                Err(error) => {
                    warn!("Failed to create telemetry shared-memory writer: {error:?}");
                    ApplicationShmState::RetryAt {
                        path,
                        deadline: now + Duration::from_secs(60),
                    }
                }
            };
        }

        if let ApplicationShmState::Ready(writer) = &self.shm_state {
            writer.write(&serialized);
        }
    }

    pub fn register_metric(&mut self, metric: MetricContext) {
        let name = metric.name.clone();
        let context_key = self.worker.register_metric_context(
            metric.name,
            metric.tags,
            metric.metric_type,
            metric.common,
            metric.namespace,
        );
        self.telemetry_metrics.insert(name, context_key);
    }

    pub fn to_telemetry_point(
        &self,
        (name, val, tags): (String, f64, Vec<Tag>),
    ) -> Option<TelemetryActions> {
        self.telemetry_metrics
            .get(&name)
            .map(|context_key| TelemetryActions::AddPoint((val, *context_key, tags)))
    }

    pub fn process_actions(
        &mut self,
        sidecar_actions: Vec<SidecarAction>,
    ) -> Vec<TelemetryActions> {
        let mut actions = vec![];
        for action in sidecar_actions {
            match action {
                SidecarAction::Telemetry(t) => actions.push(t),
                SidecarAction::AddTelemetryMetricPoint(point) => {
                    let metric_name = point.0.clone();
                    if let Some(telemetry_action) = self.to_telemetry_point(point) {
                        actions.push(telemetry_action);
                    } else {
                        warn!("Attempted to send telemetry point for unregistered metric: {metric_name}");
                    }
                }
                SidecarAction::PhpComposerTelemetryFile(_) => {} // handled separately
                SidecarAction::FfeExposureBatch(_) => {}         // handled in sidecar_server
                SidecarAction::FfeEvaluationMetrics { .. } => {} // handled in sidecar_server
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
                    let now_secs = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
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
}

impl Drop for TelemetryCachedClient {
    fn drop(&mut self) {
        if let ApplicationShmState::Ready(shm_writer) =
            std::mem::replace(&mut self.shm_state, ApplicationShmState::NotRequired)
        {
            shm_writer.write(&[]);
        }
    }
}

type ServiceString = String;
type EnvString = String;
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum TelemetryCachedClientOwner {
    Application,
    Runtime(InstanceId),
}
type TelemetryCachedClientKey = (TelemetryCachedClientOwner, ServiceString, EnvString);
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TelemetryMetricRegistrationScope {
    session_id: String,
    service: ServiceString,
    env: EnvString,
}

impl TelemetryMetricRegistrationScope {
    fn new(instance_id: &InstanceId, service: &str, env: &str) -> Self {
        Self {
            session_id: instance_id.session_id.clone(),
            service: service.to_string(),
            env: env.to_string(),
        }
    }
}

type TelemetryMetricRegistrations =
    HashMap<TelemetryMetricRegistrationScope, HashMap<String, MetricContext>>;

/// Non-terminal application batches are rejected once this many actions are pending.
/// Configuration and Stop batches are always admitted because they immediately promote and
/// complete the lifecycle transition instead of remaining buffered.
const MAX_PENDING_APPLICATION_ACTIONS: usize = 1024;

fn metric_contexts_match(left: &MetricContext, right: &MetricContext) -> bool {
    left.name == right.name
        && left.tags == right.tags
        && left.metric_type == right.metric_type
        && left.common == right.common
        && std::mem::discriminant(&left.namespace) == std::mem::discriminant(&right.namespace)
}

pub struct TelemetryCachedClientSet {
    inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedEntry>>>,
    pending: Arc<Mutex<HashMap<(ServiceString, EnvString), PendingTelemetryActions>>>,
    pending_action_limit: usize,
    /// Serializes cache replacement with the remove/retire phases of eviction.
    replacement_gate: Arc<Mutex<()>>,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for TelemetryCachedClientSet {
    fn default() -> Self {
        Self::with_cleanup(Duration::from_secs(1800))
    }
}

impl TelemetryCachedClientSet {
    fn with_cleanup(ttl: Duration) -> Self {
        let inner: Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedEntry>>> =
            Arc::new(Default::default());
        let clients = inner.clone();
        let pending: Arc<Mutex<HashMap<(ServiceString, EnvString), PendingTelemetryActions>>> =
            Arc::new(Default::default());
        let pending_actions = pending.clone();
        let replacement_gate = Arc::new(Mutex::new(()));
        let cleanup_replacement_gate = replacement_gate.clone();

        let handle = tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(60)).await;
                Self::evict_expired_entries(
                    &clients,
                    &cleanup_replacement_gate,
                    Instant::now(),
                    ttl,
                );
                pending_actions
                    .lock_or_panic()
                    .retain(|_, actions| actions.last_used.elapsed() < ttl);
            }
        });

        Self {
            inner,
            pending,
            pending_action_limit: MAX_PENDING_APPLICATION_ACTIONS,
            replacement_gate,
            cleanup_handle: Some(handle),
        }
    }

    fn evict_expired_entries(
        clients: &Arc<Mutex<HashMap<TelemetryCachedClientKey, TelemetryCachedEntry>>>,
        replacement_gate: &Arc<Mutex<()>>,
        now: Instant,
        ttl: Duration,
    ) {
        let _replacement_guard = replacement_gate.lock_or_panic();
        let removed = {
            let mut clients = clients.lock_or_panic();
            let expired = clients
                .iter()
                .filter(|(_, entry)| now.saturating_duration_since(entry.last_used) >= ttl)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            expired
                .into_iter()
                .filter_map(|key| clients.remove(&key))
                .collect::<Vec<_>>()
        };

        // The cache lock is released before retiring individual clients. The replacement gate
        // remains held, so no caller can publish a replacement named-SHM owner until every old
        // owner has relinquished its writer.
        for entry in &removed {
            if let Some(client) = entry.client.lock_or_panic().as_mut() {
                client.mark_stopping();
            }
        }
        drop(removed);
    }

    #[cfg(test)]
    fn evict_expired_at(&self, now: Instant, ttl: Duration) {
        Self::evict_expired_entries(&self.inner, &self.replacement_gate, now, ttl);
    }

    #[cfg(test)]
    fn with_pending_action_limit(limit: usize) -> Self {
        let mut clients = Self::with_cleanup(Duration::from_secs(1800));
        clients.pending_action_limit = limit;
        clients
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
            pending: Arc::clone(&self.pending),
            pending_action_limit: self.pending_action_limit,
            replacement_gate: Arc::clone(&self.replacement_gate),
            cleanup_handle: None,
        }
    }
}

impl TelemetryCachedClientSet {
    #[cfg(test)]
    fn get_existing_client(
        &self,
        service: &str,
        env: &str,
    ) -> Option<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        self.get_existing_client_with(TelemetryCachedClientOwner::Application, service, env)
    }

    fn get_existing_client_with(
        &self,
        owner: TelemetryCachedClientOwner,
        service: &str,
        env: &str,
    ) -> Option<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        let _replacement_guard = self.replacement_gate.lock_or_panic();
        let key = (owner, service.to_string(), env.to_string());

        let mut map = self.inner.lock_or_panic();
        map.get_mut(&key).map(|entry| {
            entry.last_used = Instant::now();
            entry.client.clone()
        })
    }

    fn get_or_create_with(
        &self,
        owner: TelemetryCachedClientOwner,
        service: &str,
        env: &str,
        create: impl FnOnce() -> TelemetryCachedClient,
    ) -> Arc<Mutex<Option<TelemetryCachedClient>>> {
        match self.get_or_try_create_with(owner, service, env, || Ok::<_, Infallible>(create())) {
            Ok(client) => client,
            Err(never) => match never {},
        }
    }

    fn get_or_try_create_with<E>(
        &self,
        owner: TelemetryCachedClientOwner,
        service: &str,
        env: &str,
        create: impl FnOnce() -> std::result::Result<TelemetryCachedClient, E>,
    ) -> std::result::Result<Arc<Mutex<Option<TelemetryCachedClient>>>, E> {
        let _replacement_guard = self.replacement_gate.lock_or_panic();
        let mut map = self.inner.lock_or_panic();
        let key = (owner, service.to_string(), env.to_string());
        match map.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                let active = {
                    let client = entry.get().client.lock_or_panic();
                    client.as_ref().is_some_and(|client| !client.is_stopping())
                };
                if active {
                    entry.get_mut().last_used = Instant::now();
                    Ok(entry.get().client.clone())
                } else {
                    let new_client = Arc::new(Mutex::new(Some(create()?)));
                    entry.insert(TelemetryCachedEntry {
                        last_used: Instant::now(),
                        client: new_client.clone(),
                    });
                    info!("Replaced stopped telemetry client for {key:?}");
                    Ok(new_client)
                }
            }
            Entry::Vacant(entry) => {
                let new_client = Arc::new(Mutex::new(Some(create()?)));
                entry.insert(TelemetryCachedEntry {
                    last_used: Instant::now(),
                    client: new_client.clone(),
                });
                info!("Created new telemetry client for {key:?}");
                Ok(new_client)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn get_or_create<F>(
        &self,
        metadata: TelemetryWorkerMetadata<'_>,
        get_config: F,
        initial: InitialTelemetryData,
    ) -> Result<Arc<Mutex<Option<TelemetryCachedClient>>>>
    where
        F: FnOnce() -> Config,
    {
        let service = metadata.service;
        let env = metadata.env;
        self.get_or_try_create_with(
            TelemetryCachedClientOwner::Application,
            service,
            env,
            || TelemetryCachedClient::new(metadata, get_config, initial),
        )
    }

    pub(crate) fn get_or_create_for_actions(
        &self,
        metadata: TelemetryWorkerMetadata<'_>,
        actions: Vec<PendingApplicationAction>,
        get_config: impl FnOnce() -> Config,
        initialize: impl FnOnce(
            &Arc<Mutex<Option<TelemetryCachedClient>>>,
            Vec<PendingApplicationAction>,
        ) -> bool,
    ) -> ApplicationTelemetryDispatch {
        let service = metadata.service;
        let env = metadata.env;
        let _replacement_guard = self.replacement_gate.lock_or_panic();
        let mut clients = self.inner.lock_or_panic();
        let key = (
            TelemetryCachedClientOwner::Application,
            service.to_string(),
            env.to_string(),
        );

        if let Some(entry) = clients.get_mut(&key) {
            let active = entry
                .client
                .lock_or_panic()
                .as_ref()
                .is_some_and(|client| !client.is_stopping());
            if active {
                entry.last_used = Instant::now();
                return ApplicationTelemetryDispatch::Ready {
                    client: entry.client.clone(),
                    actions,
                    created: false,
                    remove_client: false,
                };
            }
        }
        clients.remove(&key);

        let pending_key = (service.to_string(), env.to_string());
        let mut pending = self.pending.lock_or_panic();
        let pending_actions =
            pending
                .entry(pending_key.clone())
                .or_insert_with(|| PendingTelemetryActions {
                    last_used: Instant::now(),
                    actions: Vec::new(),
                });
        let incoming_promotes = actions.iter().any(|pending_action| {
            matches!(
                pending_action.action,
                SidecarAction::Telemetry(TelemetryActions::AddConfig(_))
                    | SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop))
            )
        });
        if !incoming_promotes
            && pending_actions.actions.len().saturating_add(actions.len())
                > self.pending_action_limit
        {
            warn!(
                "Rejecting {} application telemetry actions for {service:?}/{env:?}: \
                 pending lifecycle limit {} would be exceeded",
                actions.len(),
                self.pending_action_limit
            );
            return ApplicationTelemetryDispatch::Pending;
        }
        pending_actions.actions.extend(actions);
        pending_actions.last_used = Instant::now();

        let should_promote = pending_actions.actions.iter().any(|pending_action| {
            matches!(
                pending_action.action,
                SidecarAction::Telemetry(TelemetryActions::AddConfig(_))
                    | SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop))
            )
        });
        if !should_promote {
            return ApplicationTelemetryDispatch::Pending;
        }

        let Some(pending_actions) = pending.remove(&pending_key) else {
            warn!("Pending application telemetry lifecycle disappeared for {service:?}/{env:?}");
            return ApplicationTelemetryDispatch::Pending;
        };
        let mut actions = pending_actions.actions;
        drop(pending);

        let next_lifecycle_actions = actions
            .iter()
            .position(|pending_action| {
                matches!(
                    pending_action.action,
                    SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop))
                )
            })
            .filter(|stop_index| *stop_index + 1 < actions.len())
            .map(|stop_index| actions.split_off(stop_index + 1))
            .unwrap_or_default();
        let initial = InitialTelemetryData::from_pending_actions(&actions);
        match TelemetryCachedClient::new(metadata, get_config, initial) {
            Ok(mut telemetry) => {
                for pending_action in &actions {
                    match &pending_action.action {
                        SidecarAction::Telemetry(TelemetryActions::AddConfig(_)) => {
                            telemetry.shared.config_sent = true;
                        }
                        SidecarAction::Telemetry(TelemetryActions::AddIntegration(integration)) => {
                            telemetry.shared.integrations.insert(integration.clone());
                        }
                        _ => {}
                    }
                }
                telemetry.write_shm_file();
                let client = Arc::new(Mutex::new(Some(telemetry)));
                let remove_client = initialize(&client, actions);
                clients.insert(
                    key,
                    TelemetryCachedEntry {
                        last_used: Instant::now(),
                        client: client.clone(),
                    },
                );
                info!("Created new telemetry client for {service:?}/{env:?}");
                ApplicationTelemetryDispatch::Ready {
                    client,
                    actions: next_lifecycle_actions,
                    created: true,
                    remove_client,
                }
            }
            Err(error) => {
                self.pending.lock_or_panic().insert(
                    pending_key,
                    PendingTelemetryActions {
                        last_used: Instant::now(),
                        actions,
                    },
                );
                warn!("Failed to create telemetry client for {service:?}/{env:?}: {error:?}");
                ApplicationTelemetryDispatch::Pending
            }
        }
    }

    pub(crate) fn workers(&self) -> Vec<TelemetryWorkerHandle> {
        self.clients()
            .into_iter()
            .filter_map(|client| {
                client
                    .lock_or_panic()
                    .as_ref()
                    .map(|client| client.worker.clone())
            })
            .collect()
    }

    pub(crate) fn clients(&self) -> Vec<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        let clients = self.inner.lock_or_panic();
        clients.values().map(|entry| entry.client.clone()).collect()
    }

    fn remove_clients_matching(
        &self,
        predicate: impl Fn(&TelemetryCachedClientOwner, &str, &str) -> bool,
    ) {
        let _replacement_guard = self.replacement_gate.lock_or_panic();
        let removed = {
            let mut clients = self.inner.lock_or_panic();
            let keys = clients
                .keys()
                .filter(|(owner, service, env)| predicate(owner, service, env))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| clients.remove(&key))
                .collect::<Vec<_>>()
        };
        for entry in &removed {
            if let Some(mut client) = entry.client.lock_or_panic().take() {
                client.mark_stopping();
            }
        }
        drop(removed);
    }

    #[cfg(test)]
    pub(crate) fn remove_runtime(&self, instance_id: &InstanceId) {
        self.remove_clients_matching(|owner, _, _| {
            matches!(owner, TelemetryCachedClientOwner::Runtime(owner_instance) if owner_instance == instance_id)
        });
    }

    pub(crate) fn remove_session(&self, session_id: &str) {
        self.remove_clients_matching(|owner, _, _| {
            matches!(owner, TelemetryCachedClientOwner::Runtime(owner_instance) if owner_instance.session_id == session_id)
        });
    }

    pub fn remove_telemetry_client(
        &self,
        service: &str,
        env: &str,
        expected: &Arc<Mutex<Option<TelemetryCachedClient>>>,
    ) {
        self.remove_client_with(
            TelemetryCachedClientOwner::Application,
            service,
            env,
            expected,
        );
    }

    fn remove_client_with(
        &self,
        owner: TelemetryCachedClientOwner,
        service: &str,
        env: &str,
        expected: &Arc<Mutex<Option<TelemetryCachedClient>>>,
    ) {
        let _replacement_guard = self.replacement_gate.lock_or_panic();
        let key = (owner, service.to_string(), env.to_string());
        let mut clients = self.inner.lock_or_panic();
        if clients
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.client, expected))
        {
            clients.remove(&key);
        }
    }
}

pub(crate) struct MetricsLogsClientSet {
    clients: TelemetryCachedClientSet,
    registrations: Arc<Mutex<TelemetryMetricRegistrations>>,
    registration_limit: usize,
    #[cfg(test)]
    registration_snapshot_hook: Option<MetricRegistrationSnapshotHook>,
}

#[derive(Debug, Eq, PartialEq)]
enum MetricRegistrationOutcome {
    Unchanged,
    Inserted,
    Changed,
    RejectedCapacity { limit: usize },
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MetricRegistrationSnapshotHook {
    snapshot_taken: Arc<std::sync::Barrier>,
    resume_creation: Arc<std::sync::Barrier>,
}

#[cfg(test)]
impl MetricRegistrationSnapshotHook {
    pub(crate) fn new() -> Self {
        Self {
            snapshot_taken: Arc::new(std::sync::Barrier::new(2)),
            resume_creation: Arc::new(std::sync::Barrier::new(2)),
        }
    }

    fn wait(&self) {
        self.snapshot_taken.wait();
        self.resume_creation.wait();
    }

    pub(crate) fn wait_until_snapshot(&self) {
        self.snapshot_taken.wait();
    }

    pub(crate) fn resume_creation(&self) {
        self.resume_creation.wait();
    }
}

impl Default for MetricsLogsClientSet {
    fn default() -> Self {
        Self {
            clients: TelemetryCachedClientSet::default(),
            registrations: Arc::new(Default::default()),
            registration_limit: libdd_telemetry::worker::MAX_ITEMS,
            #[cfg(test)]
            registration_snapshot_hook: None,
        }
    }
}

impl Clone for MetricsLogsClientSet {
    fn clone(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            registrations: self.registrations.clone(),
            registration_limit: self.registration_limit,
            #[cfg(test)]
            registration_snapshot_hook: self.registration_snapshot_hook.clone(),
        }
    }
}

impl MetricsLogsClientSet {
    pub(crate) fn workers(&self) -> Vec<TelemetryWorkerHandle> {
        self.clients.workers()
    }

    pub(crate) fn clients(&self) -> Vec<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        self.clients.clients()
    }

    #[cfg(test)]
    pub(crate) fn remove_runtime(&self, instance_id: &InstanceId) {
        self.clients.remove_runtime(instance_id);
    }

    pub(crate) fn remove_runtimes(&self, instance_ids: &HashSet<InstanceId>) {
        self.clients.remove_clients_matching(|owner, _, _| {
            matches!(
                owner,
                TelemetryCachedClientOwner::Runtime(instance_id)
                    if instance_ids.contains(instance_id)
            )
        });
    }

    pub(crate) fn remove_session(&self, session_id: &str) {
        {
            let mut registrations = self.registrations.lock_or_panic();
            registrations.retain(|scope, _| scope.session_id != session_id);
        }
        self.clients.remove_session(session_id);
    }

    #[cfg(test)]
    fn with_registration_limit(registration_limit: usize) -> Self {
        Self {
            registration_limit,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_registration_snapshot_hook(hook: MetricRegistrationSnapshotHook) -> Self {
        Self {
            registration_snapshot_hook: Some(hook),
            ..Default::default()
        }
    }

    pub(crate) fn get_existing_metrics_logs(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
    ) -> Option<Arc<Mutex<Option<TelemetryCachedClient>>>> {
        self.clients.get_existing_client_with(
            TelemetryCachedClientOwner::Runtime(instance_id.clone()),
            service,
            env,
        )
    }

    pub(crate) fn get_or_create_metrics_logs<F>(
        &self,
        service: &str,
        env: &str,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMetadata,
        get_config: F,
        process_tags: Vec<Tag>,
    ) -> Arc<Mutex<Option<TelemetryCachedClient>>>
    where
        F: FnOnce() -> Config,
    {
        self.clients.get_or_create_with(
            TelemetryCachedClientOwner::Runtime(instance_id.clone()),
            service,
            env,
            || {
                let registrations = self.registered_metrics(instance_id, service, env);
                #[cfg(test)]
                if let Some(hook) = &self.registration_snapshot_hook {
                    hook.wait();
                }
                let mut client = TelemetryCachedClient::new_metrics_logs(
                    service,
                    env,
                    instance_id,
                    runtime_meta,
                    get_config,
                    process_tags,
                );
                for metric in registrations {
                    client.register_metric(metric);
                }
                client
            },
        )
    }

    #[cfg(test)]
    fn remove_metrics_logs_client(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
        expected: &Arc<Mutex<Option<TelemetryCachedClient>>>,
    ) {
        self.clients.remove_client_with(
            TelemetryCachedClientOwner::Runtime(instance_id.clone()),
            service,
            env,
            expected,
        );
    }

    pub(crate) fn registered_metrics(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
    ) -> Vec<MetricContext> {
        let scope = TelemetryMetricRegistrationScope::new(instance_id, service, env);
        self.registrations
            .lock_or_panic()
            .get(&scope)
            .into_iter()
            .flat_map(|metrics| metrics.values().cloned())
            .collect()
    }

    #[cfg(test)]
    fn registered_metric_names(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
    ) -> HashSet<String> {
        self.registered_metrics(instance_id, service, env)
            .into_iter()
            .map(|metric| metric.name)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn register_metric(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
        metric: MetricContext,
    ) -> bool {
        !matches!(
            self.register_metric_with_outcome(instance_id, service, env, metric),
            MetricRegistrationOutcome::RejectedCapacity { .. }
        )
    }

    fn register_metric_with_outcome(
        &self,
        instance_id: &InstanceId,
        service: &str,
        env: &str,
        metric: MetricContext,
    ) -> MetricRegistrationOutcome {
        let scope = TelemetryMetricRegistrationScope::new(instance_id, service, env);
        let mut registrations = self.registrations.lock_or_panic();
        let metrics = registrations.entry(scope).or_default();
        let outcome = match metrics.get(&metric.name) {
            Some(registered_metric) if metric_contexts_match(registered_metric, &metric) => {
                MetricRegistrationOutcome::Unchanged
            }
            Some(_) => MetricRegistrationOutcome::Changed,
            None if metrics.len() >= self.registration_limit => {
                MetricRegistrationOutcome::RejectedCapacity {
                    limit: self.registration_limit,
                }
            }
            None => MetricRegistrationOutcome::Inserted,
        };
        if matches!(
            outcome,
            MetricRegistrationOutcome::Unchanged
                | MetricRegistrationOutcome::RejectedCapacity { .. }
        ) {
            return outcome;
        }
        metrics.insert(metric.name.clone(), metric.clone());
        drop(registrations);

        if outcome == MetricRegistrationOutcome::Changed {
            self.clients
                .remove_clients_matching(|owner, client_service, client_env| {
                    matches!(
                        owner,
                        TelemetryCachedClientOwner::Runtime(owner_instance)
                            if owner_instance.session_id == instance_id.session_id
                                && client_service == service
                                && client_env == env
                    )
                });
            return outcome;
        }

        let clients = self
            .clients
            .inner
            .lock_or_panic()
            .iter()
            .filter_map(|((owner, client_service, client_env), entry)| {
                let TelemetryCachedClientOwner::Runtime(owner_instance) = owner else {
                    return None;
                };
                (owner_instance.session_id == instance_id.session_id
                    && client_service == service
                    && client_env == env)
                    .then(|| entry.client.clone())
            })
            .collect::<Vec<_>>();
        for client in clients {
            if let Some(client) = client.lock_or_panic().as_mut() {
                if !client.is_stopping() {
                    client.register_metric(metric.clone());
                }
            }
        }
        outcome
    }
}

pub fn path_for_telemetry(service: &str, env: &str) -> CString {
    let mut hasher = ZwoHasher::default();
    service.hash(&mut hasher);
    env.hash(&mut hasher);
    let hash = hasher.finish();

    let mut path = format!(
        "/ddtl{}-{}",
        primary_sidecar_identifier(),
        BASE64_URL_SAFE_NO_PAD.encode(hash.to_ne_bytes()),
    );
    path.truncate(31);

    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

pub fn get_telemetry_action_sender() -> Result<TelemetryActionSender> {
    TELEMETRY_ACTION_SENDER
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("Telemetry action sender not initialized"))
}

pub(crate) fn init_telemetry_sender(
    sidecar: &SidecarServer,
) -> Option<mpsc::Receiver<DirectTelemetryMessage>> {
    let (tx, rx) = mpsc::channel(1000);
    let sender = TelemetryActionSender { sender: tx };
    if TELEMETRY_ACTION_SENDER.set(sender.clone()).is_err() {
        warn!("Telemetry action sender already initialized");
        return None;
    }
    sidecar.install_direct_telemetry_sender(sender);
    Some(rx)
}

fn get_telemetry_client(
    sidecar: &SidecarServer,
    instance_id: &InstanceId,
    service_name: &str,
    env_name: &str,
) -> Option<Arc<Mutex<Option<TelemetryCachedClient>>>> {
    if let Some(existing) =
        sidecar
            .metrics_logs_clients
            .get_existing_metrics_logs(instance_id, service_name, env_name)
    {
        return Some(existing);
    }

    let session = sidecar.find_session(&instance_id.session_id)?;
    sidecar.find_runtime(instance_id)?;
    let trace_config = session.get_trace_config();
    let runtime_meta = RuntimeMetadata::new(
        trace_config.language.as_str(),
        trace_config.language_version.as_str(),
        trace_config.tracer_version.as_str(),
    );

    let session_config = session.session_config.lock_or_panic().as_ref().cloned();
    let Some(session_config) = session_config else {
        // Session config not yet available (need to wait for set_session_config IPC)
        return None;
    };

    let process_tags = session.process_tags_with_svc_source();

    Some(sidecar.metrics_logs_clients.get_or_create_metrics_logs(
        service_name,
        env_name,
        instance_id,
        &runtime_meta,
        move || session_config,
        process_tags,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_ipc::one_way_shared_memory::{open_named_shm, OneWayShmReader};
    use httpmock::{Method::POST, MockServer};
    use libdd_telemetry::data::{Configuration, ConfigurationOrigin, Log, LogLevel};
    use libdd_telemetry::worker::{LifecycleAction, LogIdentifier};
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Barrier;
    use tokio::time::{sleep, timeout};

    const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

    fn test_config(server: &MockServer) -> Config {
        let mut config = Config::default();
        config
            .set_endpoint_uri(server.url("/").parse().unwrap())
            .unwrap();
        config
    }

    fn initial_configuration(name: &str) -> Configuration {
        Configuration {
            name: name.to_string(),
            value: "present".to_string(),
            origin: ConfigurationOrigin::Default,
            config_id: None,
            seq_id: None,
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn application_shm_writer_retries_and_publishes_current_state() {
        const SERVICE: &str = "shm-retry";
        const ENV: &str = "test";

        let retry_at = Instant::now();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_factory = attempts.clone();
        let path = path_for_telemetry(SERVICE, ENV);
        let mut client = TelemetryCachedClient::new_with_shm_factory(
            TelemetryWorkerMetadata::new(
                SERVICE,
                ENV,
                &InstanceId::new("session", "runtime"),
                &RuntimeMetadata::new("php", "8.3", "test"),
                Vec::new(),
            ),
            Config::default,
            InitialTelemetryData::default(),
            retry_at,
            move |_| {
                attempts_for_factory.fetch_add(1, Ordering::Relaxed);
                Err(std::io::Error::other("injected failure"))
            },
        )
        .unwrap();
        client.shared.config_sent = true;

        client.write_shm_file_at(retry_at + Duration::from_secs(59), |_| {
            panic!("retry happened before the deadline")
        });
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        client.write_shm_file_at(retry_at + Duration::from_secs(60), |path| {
            OneWayShmWriter::<NamedShmHandle>::new(path.clone())
        });
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        let mut reader = OneWayShmReader::new(open_named_shm(&path).unwrap(), ());
        let shared: TelemetryCachedClientShmData = bincode::deserialize(reader.read().1).unwrap();
        assert!(shared.config_sent);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn pending_application_actions_are_bounded_without_dropping_terminal_promotion() {
        const SERVICE: &str = "bounded-pending";
        const ENV: &str = "test";
        const LIMIT: usize = 2;

        let clients = TelemetryCachedClientSet::with_pending_action_limit(LIMIT);
        let instance = InstanceId::new("session", "runtime");
        let runtime = RuntimeMetadata::new("php", "8.3", "test");
        let dependency = |name: &str| {
            SidecarAction::Telemetry(TelemetryActions::AddDependency(data::Dependency {
                name: name.to_string(),
                version: None,
            }))
        };

        for name in ["one", "two", "overflow"] {
            let dispatch = clients.get_or_create_for_actions(
                TelemetryWorkerMetadata::new(SERVICE, ENV, &instance, &runtime, Vec::new()),
                PendingApplicationAction::from_actions(
                    &instance,
                    vec![dependency(name)],
                    &HashMap::new(),
                ),
                Config::default,
                |_, _| panic!("a non-terminal batch must remain pending"),
            );
            assert!(matches!(dispatch, ApplicationTelemetryDispatch::Pending));
        }

        let pending = clients.pending.lock_or_panic();
        let stored = &pending
            .get(&(SERVICE.to_string(), ENV.to_string()))
            .expect("bounded pending lifecycle")
            .actions;
        assert_eq!(stored.len(), LIMIT);
        assert_eq!(
            stored
                .iter()
                .map(|action| match &action.action {
                    SidecarAction::Telemetry(TelemetryActions::AddDependency(dependency)) =>
                        dependency.name.as_str(),
                    _ => panic!("only dependencies should be buffered"),
                })
                .collect::<Vec<_>>(),
            ["one", "two"],
            "overflow rejects the whole non-terminal batch without disturbing prior startup data"
        );
        drop(pending);

        let promoted = Arc::new(Mutex::new(Vec::new()));
        let promoted_for_initializer = promoted.clone();
        let dispatch = clients.get_or_create_for_actions(
            TelemetryWorkerMetadata::new(SERVICE, ENV, &instance, &runtime, Vec::new()),
            PendingApplicationAction::from_actions(
                &instance,
                vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                    LifecycleAction::Stop,
                ))],
                &HashMap::new(),
            ),
            Config::default,
            move |_, actions| {
                promoted_for_initializer.lock_or_panic().extend(
                    actions
                        .into_iter()
                        .map(|pending_action| pending_action.action),
                );
                true
            },
        );
        assert!(matches!(
            dispatch,
            ApplicationTelemetryDispatch::Ready { created: true, .. }
        ));
        let promoted = promoted.lock_or_panic();
        assert_eq!(promoted.len(), LIMIT + 1);
        assert!(matches!(
            promoted.last(),
            Some(SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                LifecycleAction::Stop
            )))
        ));
    }

    fn internal_log(message: &str) -> InternalTelemetryAction {
        InternalTelemetryAction::TelemetryAction(TelemetryActions::AddLog((
            LogIdentifier { identifier: 1 },
            Log {
                message: message.to_string(),
                level: LogLevel::Debug,
                count: 1,
                stack_trace: None,
                tags: String::new(),
                is_sensitive: false,
                is_crash: false,
            },
        )))
    }

    fn metric(name: &str) -> MetricContext {
        MetricContext {
            name: name.to_string(),
            tags: Vec::new(),
            metric_type: libdd_telemetry::data::metrics::MetricType::Count,
            common: true,
            namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
        }
    }

    #[derive(Clone)]
    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock_or_panic().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn metric_capacity_rejection_is_not_logged_as_registration_success() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer_capture = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || CapturedLogWriter(writer_capture.clone()))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let mut sidecar = SidecarServer::default();
        sidecar.metrics_logs_clients = MetricsLogsClientSet::with_registration_limit(1);
        let instance = InstanceId::new("capacity-session", "capacity-runtime");

        deliver_batch(
            vec![
                InternalTelemetryAction::RegisterTelemetryMetric(metric("accepted.metric")),
                InternalTelemetryAction::RegisterTelemetryMetric(metric("rejected.metric")),
            ],
            &sidecar,
            &instance,
            "capacity-service",
            "capacity-env",
        )
        .await;

        let output = String::from_utf8(captured.lock_or_panic().clone()).expect("UTF-8 logs");
        assert!(output.contains(
            "Registered telemetry metric: session=capacity-session \
             service=capacity-service env=capacity-env metric=accepted.metric"
        ));
        assert!(output.contains(
            "Rejected telemetry metric registration: session=capacity-session \
             service=capacity-service env=capacity-env metric=rejected.metric capacity=1"
        ));
        assert!(!output.contains(
            "Registered telemetry metric: session=capacity-session \
             service=capacity-service env=capacity-env metric=rejected.metric"
        ));
    }

    async fn metric_context_count(worker: &TelemetryWorkerHandle) -> u32 {
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .expect("metric worker should collect stats");
        rx.await
            .expect("metric worker should return stats")
            .metric_contexts
    }

    fn metric_context_key(
        client: &Arc<Mutex<Option<TelemetryCachedClient>>>,
        metric_name: &str,
    ) -> ContextKey {
        let action = client
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .to_telemetry_point((metric_name.to_string(), 1.0, Vec::new()))
            .expect("registered metric should produce a point");
        let TelemetryActions::AddPoint((_, context_key, _)) = action else {
            panic!("metric point should use an AddPoint action");
        };
        context_key
    }

    #[tokio::test]
    async fn deferred_batches_are_scoped_by_instance() {
        let instance_a = InstanceId::new("session", "runtime-a");
        let instance_b = InstanceId::new("session", "runtime-b");
        let mut pending = vec![PerClientTelemetryBatch {
            key: (
                instance_a.clone(),
                "shared-service".to_string(),
                "test".to_string(),
            ),
            actions: VecDeque::from([InternalTelemetryActions {
                instance_id: instance_a,
                service_name: "shared-service".to_string(),
                env_name: "test".to_string(),
                actions: vec![internal_log("owner-a")],
            }]),
            attempts_left: 2,
            next_attempt_at: TokioInstant::now() + Duration::from_secs(60),
        }];
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(DirectTelemetryMessage::Actions(InternalTelemetryActions {
            instance_id: instance_b.clone(),
            service_name: "shared-service".to_string(),
            env_name: "test".to_string(),
            actions: vec![internal_log("owner-b")],
        }))
        .await
        .unwrap();

        let batch = next_entry(&mut pending, &mut rx)
            .await
            .expect("second owner should remain a fresh batch");
        let ReceiverEntry::Batch(TelemetryBatch::Fresh(batch)) = batch else {
            panic!("different owners must not share a deferred batch");
        };
        assert_eq!(batch.instance_id, instance_b);
        assert_eq!(pending[0].actions.len(), 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn internal_log_after_app_stop_uses_metrics_logs_worker() {
        const SERVICE: &str = "internal-before-config";
        const ENV: &str = "test";
        const LOG_MESSAGE: &str = "queued before configuration";

        let http_server = MockServer::start_async().await;
        let app_started_with_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_includes("\"name\":\"initial_config\"");
                then.status(202);
            })
            .await;
        let app_started_without_config = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"")
                    .body_excludes("\"name\":\"initial_config\"");
                then.status(202);
            })
            .await;
        let log_request = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes(LOG_MESSAGE);
                then.status(202);
            })
            .await;
        let app_closing = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-closing\"");
                then.status(202);
            })
            .await;

        let sidecar = SidecarServer::default();
        let instance_id = InstanceId::new("session", "runtime");
        let session = sidecar.get_session(&instance_id.session_id);
        session.get_runtime(&instance_id.runtime_id);
        *session.session_config.lock_or_panic() = Some(test_config(&http_server));
        let app_client = sidecar
            .telemetry_clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &instance_id,
                    &RuntimeMetadata::new("php", "8.3", "test"),
                    Vec::new(),
                ),
                || test_config(&http_server),
                InitialTelemetryData {
                    configurations: vec![initial_configuration("initial_config")],
                    ..Default::default()
                },
            )
            .expect("application telemetry worker");
        let app_worker = {
            let client = app_client.lock_or_panic();
            client
                .as_ref()
                .expect("app telemetry client")
                .worker
                .clone()
        };
        app_worker.send_stop().unwrap();
        sidecar
            .telemetry_clients
            .remove_telemetry_client(SERVICE, ENV, &app_client);

        let batch = TelemetryBatch::Fresh(InternalTelemetryActions {
            instance_id: instance_id.clone(),
            service_name: SERVICE.to_string(),
            env_name: ENV.to_string(),
            actions: vec![internal_log(LOG_MESSAGE)],
        });

        let metrics_logs_client = batch
            .get_client(&sidecar)
            .expect("session config should allow a metrics/logs client");
        assert!(!Arc::ptr_eq(&app_client, &metrics_logs_client));
        let worker = metrics_logs_client
            .lock_or_panic()
            .as_ref()
            .expect("metrics/logs telemetry client")
            .worker
            .clone();

        batch.deliver(&sidecar).await;
        worker
            .send_msg(TelemetryActions::Lifecycle(
                LifecycleAction::FlushMetricAggr,
            ))
            .await
            .unwrap();
        worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
            .await
            .unwrap();
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .unwrap();
        rx.await.unwrap();

        timeout(Duration::from_secs(5), async {
            while app_started_with_config.calls_async().await != 1
                || log_request.calls_async().await != 1
                || app_closing.calls_async().await != 1
            {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("app lifecycle and late internal log should arrive");

        assert_eq!(app_started_without_config.calls_async().await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[cfg_attr(miri, ignore)]
    async fn concurrent_same_key_creates_one_worker() {
        const CALLERS: usize = 32;
        const SERVICE: &str = "concurrent-client-creation";
        const ENV: &str = "test";

        let http_server = MockServer::start_async().await;
        let app_started = http_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"request_type\":\"app-started\"");
                then.status(202);
            })
            .await;
        let clients = TelemetryCachedClientSet::default();
        let barrier = Arc::new(Barrier::new(CALLERS));
        let config = test_config(&http_server);

        let tasks = (0..CALLERS).map(|index| {
            let clients = clients.clone();
            let barrier = barrier.clone();
            let config = config.clone();
            tokio::spawn(async move {
                let instance_id = InstanceId::new("session", &format!("runtime-{index}"));
                barrier.wait().await;
                clients
                    .get_or_create(
                        TelemetryWorkerMetadata::new(
                            SERVICE,
                            ENV,
                            &instance_id,
                            &RuntimeMetadata::new("php", "8.3", "test"),
                            Vec::new(),
                        ),
                        || config,
                        InitialTelemetryData {
                            configurations: vec![initial_configuration("concurrent_config")],
                            ..Default::default()
                        },
                    )
                    .expect("concurrent application telemetry worker")
            })
        });
        let returned_clients = futures::future::join_all(tasks)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();

        let first = &returned_clients[0];
        assert!(
            returned_clients
                .iter()
                .all(|client| Arc::ptr_eq(first, client)),
            "all same-key callers should receive the same telemetry client"
        );
        assert_eq!(clients.inner.lock_or_panic().len(), 1);

        timeout(Duration::from_secs(5), async {
            while app_started.calls_async().await != 1 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exactly one app-started request should arrive");
        assert_eq!(app_started.calls_async().await, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn promotion_initialization_precedes_visible_duplicate_integration() {
        const SERVICE: &str = "atomic-promotion";
        const ENV: &str = "test";

        let clients = TelemetryCachedClientSet::default();
        let initial_instance = InstanceId::new("session", "initial-runtime");
        let duplicate_instance = InstanceId::new("session", "duplicate-runtime");
        let integration = data::Integration {
            name: "initial-integration".to_string(),
            enabled: true,
            version: None,
            compatible: None,
            auto_enabled: None,
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (started_tx, started_rx) = std::sync::mpsc::channel();

        let initial_clients = clients.clone();
        let initial_barrier = barrier.clone();
        let initial_integration = integration.clone();
        let initial = tokio::task::spawn_blocking(move || {
            initial_clients.get_or_create_for_actions(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &initial_instance,
                    &RuntimeMetadata::new("php", "8.3", "test"),
                    Vec::new(),
                ),
                PendingApplicationAction::from_actions(
                    &initial_instance,
                    vec![
                        SidecarAction::Telemetry(TelemetryActions::AddIntegration(
                            initial_integration,
                        )),
                        SidecarAction::Telemetry(TelemetryActions::AddConfig(
                            initial_configuration("initial-config"),
                        )),
                    ],
                    &HashMap::new(),
                ),
                Config::default,
                move |_, _| {
                    started_tx
                        .send(())
                        .expect("test receiver should be available");
                    initial_barrier.wait();
                    false
                },
            )
        });

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("promotion initializer should begin");

        let duplicate_clients = clients.clone();
        let duplicate_integration = integration.clone();
        let duplicate = tokio::task::spawn_blocking(move || {
            duplicate_clients.get_or_create_for_actions(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &duplicate_instance,
                    &RuntimeMetadata::new("php", "8.3", "test"),
                    Vec::new(),
                ),
                PendingApplicationAction::from_actions(
                    &duplicate_instance,
                    vec![SidecarAction::Telemetry(TelemetryActions::AddIntegration(
                        duplicate_integration,
                    ))],
                    &HashMap::new(),
                ),
                Config::default,
                |_, _| panic!("active client should not be initialized again"),
            )
        });

        sleep(Duration::from_millis(50)).await;
        assert!(
            !duplicate.is_finished(),
            "a duplicate integration must wait until startup initialization completes"
        );
        barrier.wait();

        let initial = initial.await.expect("initial promotion task");
        assert!(matches!(
            initial,
            ApplicationTelemetryDispatch::Ready { created: true, .. }
        ));
        let ApplicationTelemetryDispatch::Ready {
            client,
            actions,
            created,
            ..
        } = duplicate.await.expect("duplicate task")
        else {
            panic!("duplicate integration should find the published client");
        };
        assert!(!created);
        assert_eq!(actions.len(), 1);
        assert!(client
            .lock_or_panic()
            .as_ref()
            .expect("published telemetry client")
            .shared
            .integrations
            .contains(&integration));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn stopping_client_is_atomically_replaced() {
        const SERVICE: &str = "stale-removal";
        const ENV: &str = "test";

        let clients = TelemetryCachedClientSet::default();
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let old = clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &InstanceId::new("session", "old"),
                    &runtime_metadata,
                    Vec::new(),
                ),
                Config::default,
                InitialTelemetryData::default(),
            )
            .expect("old application telemetry worker");

        old.lock_or_panic()
            .as_mut()
            .expect("old telemetry client")
            .mark_stopping();
        let replacement = clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &InstanceId::new("session", "replacement"),
                    &runtime_metadata,
                    Vec::new(),
                ),
                Config::default,
                InitialTelemetryData::default(),
            )
            .expect("replacement application telemetry worker");
        assert!(!Arc::ptr_eq(&old, &replacement));
        const REPLACEMENT_STATE: &[u8] = b"replacement state";
        {
            let replacement_client = replacement.lock_or_panic();
            let ApplicationShmState::Ready(shm_writer) = &replacement_client
                .as_ref()
                .expect("replacement telemetry client")
                .shm_state
            else {
                panic!("replacement shared-memory writer should be ready");
            };
            shm_writer.write(REPLACEMENT_STATE);
        }

        old.lock_or_panic().take();
        let mut reader = OneWayShmReader::new(
            open_named_shm(&path_for_telemetry(SERVICE, ENV))
                .expect("replacement shared-memory name should remain available"),
            (),
        );
        assert_eq!(reader.read().1, REPLACEMENT_STATE);

        clients.remove_telemetry_client(SERVICE, ENV, &old);

        let cached = clients
            .get_existing_client(SERVICE, ENV)
            .expect("replacement client should remain cached");
        assert!(Arc::ptr_eq(&replacement, &cached));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ttl_eviction_retires_application_shm_before_replacement() {
        const SERVICE: &str = "ttl-shm-retirement";
        const ENV: &str = "test";
        const REPLACEMENT_STATE: &[u8] = b"replacement survives old owner";

        let clients = TelemetryCachedClientSet::with_cleanup(Duration::from_secs(60));
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let old = clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &InstanceId::new("session", "old-runtime"),
                    &runtime_metadata,
                    Vec::new(),
                ),
                Config::default,
                InitialTelemetryData::default(),
            )
            .expect("old application telemetry worker");
        let retained_old_owner = old.clone();
        let now = Instant::now();
        clients
            .inner
            .lock_or_panic()
            .values_mut()
            .for_each(|entry| entry.last_used = now - Duration::from_secs(61));

        clients.evict_expired_at(now, Duration::from_secs(60));
        assert!(old
            .lock_or_panic()
            .as_ref()
            .expect("externally retained client")
            .is_stopping());

        let replacement = clients
            .get_or_create(
                TelemetryWorkerMetadata::new(
                    SERVICE,
                    ENV,
                    &InstanceId::new("session", "replacement-runtime"),
                    &runtime_metadata,
                    Vec::new(),
                ),
                Config::default,
                InitialTelemetryData::default(),
            )
            .expect("replacement application telemetry worker");
        {
            let replacement = replacement.lock_or_panic();
            let ApplicationShmState::Ready(writer) =
                &replacement.as_ref().expect("replacement client").shm_state
            else {
                panic!("replacement shared-memory writer should be ready");
            };
            writer.write(REPLACEMENT_STATE);
        }

        drop(retained_old_owner);
        old.lock_or_panic().take();

        let mut reader = OneWayShmReader::new(
            open_named_shm(&path_for_telemetry(SERVICE, ENV))
                .expect("replacement shared-memory name should remain available"),
            (),
        );
        assert_eq!(reader.read().1, REPLACEMENT_STATE);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn metrics_logs_cache_replays_registrations_after_eviction() {
        const SERVICE: &str = "persistent-metrics";
        const ENV: &str = "test";
        const METRIC: &str = "persistent.metric";

        let http_server = MockServer::start_async().await;
        let clients = MetricsLogsClientSet::default();
        let instance_id = InstanceId::new("session", "runtime");

        let client = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance_id,
            &RuntimeMetadata::new("php", "8.3", "test"),
            || test_config(&http_server),
            Vec::new(),
        );
        clients.register_metric(
            &instance_id,
            SERVICE,
            ENV,
            MetricContext {
                name: METRIC.to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            },
        );
        let stale_last_used = Instant::now();
        sleep(Duration::from_millis(1)).await;
        clients
            .clients
            .inner
            .lock_or_panic()
            .get_mut(&(
                TelemetryCachedClientOwner::Runtime(instance_id.clone()),
                SERVICE.to_string(),
                ENV.to_string(),
            ))
            .expect("cached entry")
            .last_used = stale_last_used;

        let cached = clients
            .get_existing_metrics_logs(&instance_id, SERVICE, ENV)
            .expect("persistent cache entry");
        assert!(Arc::ptr_eq(&client, &cached));
        assert!(cached
            .lock_or_panic()
            .as_ref()
            .expect("metrics/logs client")
            .telemetry_metrics
            .contains_key(METRIC));
        assert!(
            clients
                .clients
                .inner
                .lock_or_panic()
                .get(&(
                    TelemetryCachedClientOwner::Runtime(instance_id.clone()),
                    SERVICE.to_string(),
                    ENV.to_string(),
                ))
                .expect("cached entry")
                .last_used
                > stale_last_used
        );

        clients.remove_metrics_logs_client(&instance_id, SERVICE, ENV, &client);
        let replacement = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance_id,
            &RuntimeMetadata::new("php", "8.3", "test"),
            || test_config(&http_server),
            Vec::new(),
        );
        assert!(!Arc::ptr_eq(&client, &replacement));
        assert!(replacement
            .lock_or_panic()
            .as_ref()
            .expect("replacement metrics/logs client")
            .telemetry_metrics
            .contains_key(METRIC));
        assert_eq!(
            clients.registered_metric_names(&instance_id, SERVICE, ENV),
            HashSet::from([METRIC.to_string()])
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn metric_registration_is_broadcast_to_existing_matching_runtimes() {
        const SERVICE: &str = "shared-appsec-service";
        const ENV: &str = "prod";
        const METRIC: &str = "waf.requests";

        let server = MockServer::start_async().await;
        let clients = MetricsLogsClientSet::default();
        let runtime_meta = RuntimeMetadata::new("php", "8.3", "test");
        let runtime_a = InstanceId::new("session", "runtime-a");
        let runtime_b = InstanceId::new("session", "runtime-b");
        let other_session = InstanceId::new("other-session", "runtime-c");
        let other_service = InstanceId::new("session", "runtime-d");
        let other_env = InstanceId::new("session", "runtime-e");

        let client_a = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &runtime_a,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        let client_b = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &runtime_b,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        let client_other_session = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &other_session,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        let client_other_service = clients.get_or_create_metrics_logs(
            "other-service",
            ENV,
            &other_service,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        let client_other_env = clients.get_or_create_metrics_logs(
            SERVICE,
            "other-env",
            &other_env,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );

        assert!(clients.register_metric(
            &runtime_a,
            SERVICE,
            ENV,
            MetricContext {
                name: METRIC.to_string(),
                tags: Vec::new(),
                metric_type: libdd_telemetry::data::metrics::MetricType::Count,
                common: true,
                namespace: libdd_telemetry::data::metrics::MetricNamespace::Appsec,
            },
        ));

        for client in [&client_a, &client_b] {
            assert!(client
                .lock_or_panic()
                .as_ref()
                .expect("runtime worker")
                .to_telemetry_point((METRIC.to_string(), 1.0, Vec::new()))
                .is_some());
        }
        for client in [
            &client_other_session,
            &client_other_service,
            &client_other_env,
        ] {
            assert!(client
                .lock_or_panic()
                .as_ref()
                .expect("nonmatching runtime worker")
                .to_telemetry_point((METRIC.to_string(), 1.0, Vec::new()))
                .is_none());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[cfg_attr(miri, ignore)]
    async fn metric_registration_during_worker_creation_is_replayed() {
        const SERVICE: &str = "creation-race-service";
        const ENV: &str = "test";
        const METRIC: &str = "creation.race.metric";

        let hook = MetricRegistrationSnapshotHook::new();
        let clients = MetricsLogsClientSet::with_registration_snapshot_hook(hook.clone());
        let instance_id = InstanceId::new("session", "runtime");
        let creating_clients = clients.clone();
        let creating_instance = instance_id.clone();
        let creation = tokio::task::spawn_blocking(move || {
            creating_clients.get_or_create_metrics_logs(
                SERVICE,
                ENV,
                &creating_instance,
                &RuntimeMetadata::new("php", "8.3", "test"),
                Config::default,
                Vec::new(),
            )
        });

        hook.snapshot_taken.wait();
        let registering_clients = clients.clone();
        let registering_instance = instance_id.clone();
        let registration = tokio::task::spawn_blocking(move || {
            registering_clients.register_metric(&registering_instance, SERVICE, ENV, metric(METRIC))
        });
        timeout(Duration::from_secs(1), async {
            while !clients
                .registered_metric_names(&instance_id, SERVICE, ENV)
                .contains(METRIC)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("registration should be stored before worker creation resumes");

        hook.resume_creation.wait();
        let client = creation.await.expect("worker creation task");
        assert!(registration.await.expect("registration task"));
        assert!(client
            .lock_or_panic()
            .as_ref()
            .expect("new runtime worker")
            .to_telemetry_point((METRIC.to_string(), 1.0, Vec::new()))
            .is_some());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn metric_registrations_do_not_cross_sessions() {
        const SERVICE: &str = "shared-appsec-service";
        const ENV: &str = "prod";
        const METRIC: &str = "waf.requests";

        let server = MockServer::start_async().await;
        let clients = MetricsLogsClientSet::default();
        let runtime_meta = RuntimeMetadata::new("php", "8.3", "test");
        let runtime_a = InstanceId::new("session-a", "runtime-a");
        let runtime_b = InstanceId::new("session-b", "runtime-b");

        let client_a = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &runtime_a,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        assert!(clients.register_metric(&runtime_a, SERVICE, ENV, metric(METRIC)));
        assert!(client_a
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .telemetry_metrics
            .contains_key(METRIC));

        let client_b = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &runtime_b,
            &runtime_meta,
            || test_config(&server),
            Vec::new(),
        );
        assert!(!client_b
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .telemetry_metrics
            .contains_key(METRIC));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn runtime_and_session_cleanup_remove_owned_state() {
        const SERVICE: &str = "cleanup-service";
        const ENV: &str = "test";

        let clients = MetricsLogsClientSet::default();
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let session_a_runtime_a = InstanceId::new("session-a", "runtime-a");
        let session_a_runtime_b = InstanceId::new("session-a", "runtime-b");
        let session_b_runtime = InstanceId::new("session-b", "runtime");

        for instance_id in [
            &session_a_runtime_a,
            &session_a_runtime_b,
            &session_b_runtime,
        ] {
            clients.get_or_create_metrics_logs(
                SERVICE,
                ENV,
                instance_id,
                &runtime_metadata,
                Config::default,
                Vec::new(),
            );
        }
        assert!(clients.register_metric(
            &session_a_runtime_a,
            SERVICE,
            ENV,
            metric("cleanup.metric"),
        ));
        assert!(clients.register_metric(
            &session_b_runtime,
            SERVICE,
            ENV,
            metric("cleanup.metric"),
        ));

        clients.remove_runtime(&session_a_runtime_a);
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_a, SERVICE, ENV)
            .is_none());
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_b, SERVICE, ENV)
            .is_some());
        clients.remove_runtime(&session_a_runtime_a);
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_a, SERVICE, ENV)
            .is_none());
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_b, SERVICE, ENV)
            .is_some());

        clients.remove_session("session-a");
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_b, SERVICE, ENV)
            .is_none());
        assert!(clients
            .get_existing_metrics_logs(&session_b_runtime, SERVICE, ENV)
            .is_some());
        assert!(clients
            .registered_metrics(&session_a_runtime_b, SERVICE, ENV)
            .is_empty());
        clients.remove_session("session-a");
        assert!(clients
            .get_existing_metrics_logs(&session_a_runtime_b, SERVICE, ENV)
            .is_none());
        assert!(clients
            .registered_metrics(&session_a_runtime_b, SERVICE, ENV)
            .is_empty());
        assert!(clients
            .get_existing_metrics_logs(&session_b_runtime, SERVICE, ENV)
            .is_some());
        assert!(!clients
            .registered_metrics(&session_b_runtime, SERVICE, ENV)
            .is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn identical_metric_registration_keeps_the_existing_worker_context() {
        const SERVICE: &str = "identical-metric-service";
        const ENV: &str = "test";
        const METRIC: &str = "identical.metric";

        let server = MockServer::start_async().await;
        let clients = MetricsLogsClientSet::default();
        let instance = InstanceId::new("session", "runtime");
        let client = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance,
            &RuntimeMetadata::new("php", "8.3", "test"),
            || test_config(&server),
            Vec::new(),
        );
        let worker = client
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .worker
            .clone();
        let initial_context_count = metric_context_count(&worker).await;

        assert!(clients.register_metric(&instance, SERVICE, ENV, metric(METRIC)));
        let first_context = metric_context_key(&client, METRIC);
        let registered_context_count = metric_context_count(&worker).await;
        assert_eq!(registered_context_count, initial_context_count + 1);

        assert!(clients.register_metric(&instance, SERVICE, ENV, metric(METRIC)));
        assert_eq!(metric_context_key(&client, METRIC), first_context);
        assert_eq!(
            metric_context_count(&worker).await,
            registered_context_count
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn changed_metric_definitions_roll_workers_without_context_growth() {
        const SERVICE: &str = "changed-metric-service";
        const ENV: &str = "test";
        const METRIC: &str = "changing.metric";

        let clients = MetricsLogsClientSet::default();
        let instance = InstanceId::new("session", "runtime");
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let mut current = clients.get_or_create_metrics_logs(
            SERVICE,
            ENV,
            &instance,
            &runtime_metadata,
            Config::default,
            Vec::new(),
        );

        for revision in 0..20 {
            let previous = current.clone();
            let mut definition = metric(METRIC);
            definition.metric_type = if revision % 2 == 0 {
                libdd_telemetry::data::metrics::MetricType::Count
            } else {
                libdd_telemetry::data::metrics::MetricType::Gauge
            };
            assert!(clients.register_metric(&instance, SERVICE, ENV, definition));
            current = clients.get_or_create_metrics_logs(
                SERVICE,
                ENV,
                &instance,
                &runtime_metadata,
                Config::default,
                Vec::new(),
            );

            if revision > 0 {
                assert!(
                    previous.lock_or_panic().is_none(),
                    "a changed definition should synchronously retire the old worker"
                );
            }
            let worker = current
                .lock_or_panic()
                .as_ref()
                .expect("replacement runtime worker")
                .worker
                .clone();
            assert_eq!(
                metric_context_count(&worker).await,
                1,
                "each replacement replays only the latest definition for each name"
            );
        }

        let definitions = clients.registered_metrics(&instance, SERVICE, ENV);
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].metric_type,
            libdd_telemetry::data::metrics::MetricType::Gauge
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn full_metric_scope_preserves_existing_definitions() {
        let clients = MetricsLogsClientSet::with_registration_limit(2);
        let instance = InstanceId::new("session", "runtime");
        let server = MockServer::start_async().await;
        let gauge_metric = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("\"metric\":\"one\"")
                    .body_includes("\"type\":\"gauge\"");
                then.status(202);
            })
            .await;
        clients.get_or_create_metrics_logs(
            "service",
            "env",
            &instance,
            &RuntimeMetadata::new("php", "8.3", "test"),
            || test_config(&server),
            Vec::new(),
        );

        assert!(clients.register_metric(&instance, "service", "env", metric("one")));
        assert!(clients.register_metric(&instance, "service", "env", metric("two")));
        let mut updated_metric = metric("one");
        updated_metric.metric_type = libdd_telemetry::data::metrics::MetricType::Gauge;
        assert!(clients.register_metric(&instance, "service", "env", updated_metric));
        assert!(!clients.register_metric(&instance, "service", "env", metric("three")));
        let names = clients.registered_metric_names(&instance, "service", "env");
        assert_eq!(names, HashSet::from(["one".to_string(), "two".to_string()]));
        let client = clients.get_or_create_metrics_logs(
            "service",
            "env",
            &instance,
            &RuntimeMetadata::new("php", "8.3", "test"),
            || test_config(&server),
            Vec::new(),
        );
        let point = client
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .to_telemetry_point(("one".to_string(), 1.0, Vec::new()))
            .expect("updated metric should produce a point");
        let worker = client
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .worker
            .clone();
        worker.send_msg(point).await.unwrap();
        worker
            .send_msg(TelemetryActions::Lifecycle(
                LifecycleAction::FlushMetricAggr,
            ))
            .await
            .unwrap();
        worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
            .await
            .unwrap();
        let (tx, rx) = futures::channel::oneshot::channel();
        worker
            .send_msg(TelemetryActions::CollectStats(tx))
            .await
            .unwrap();
        rx.await.unwrap();
        timeout(Duration::from_secs(5), async {
            while gauge_metric.calls_async().await != 1 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("updated metric should be delivered as a gauge");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn metrics_logs_cache_is_scoped_by_instance() {
        const SERVICE: &str = "shared-service";
        const ENV: &str = "test";

        let server_a = MockServer::start_async().await;
        let server_b = MockServer::start_async().await;
        let expected_a = server_a
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("owner-a");
                then.status(202);
            })
            .await;
        let unexpected_a = server_a
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("owner-b");
                then.status(202);
            })
            .await;
        let expected_b = server_b
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("owner-b");
                then.status(202);
            })
            .await;
        let unexpected_b = server_b
            .mock_async(|when, then| {
                when.method(POST)
                    .path(TELEMETRY_PATH)
                    .body_includes("owner-a");
                then.status(202);
            })
            .await;

        let sidecar = SidecarServer::default();
        let instance_a = InstanceId::new("session-a", "runtime-a");
        let instance_b = InstanceId::new("session-b", "runtime-b");
        *sidecar
            .get_session(&instance_a.session_id)
            .session_config
            .lock_or_panic() = Some(test_config(&server_a));
        *sidecar
            .get_session(&instance_b.session_id)
            .session_config
            .lock_or_panic() = Some(test_config(&server_b));
        sidecar
            .get_session(&instance_a.session_id)
            .get_runtime(&instance_a.runtime_id);
        sidecar
            .get_session(&instance_b.session_id)
            .get_runtime(&instance_b.runtime_id);

        let client_a =
            get_telemetry_client(&sidecar, &instance_a, SERVICE, ENV).expect("first owner");
        let client_b =
            get_telemetry_client(&sidecar, &instance_b, SERVICE, ENV).expect("second owner");
        assert!(!Arc::ptr_eq(&client_a, &client_b));

        for (instance_id, client, message) in [
            (&instance_a, &client_a, "owner-a"),
            (&instance_b, &client_b, "owner-b"),
        ] {
            let worker = client
                .lock_or_panic()
                .as_ref()
                .expect("metrics/logs client")
                .worker
                .clone();
            TelemetryBatch::Fresh(InternalTelemetryActions {
                instance_id: instance_id.clone(),
                service_name: SERVICE.to_string(),
                env_name: ENV.to_string(),
                actions: vec![internal_log(message)],
            })
            .deliver(&sidecar)
            .await;
            worker
                .send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData))
                .await
                .unwrap();
            let (tx, rx) = futures::channel::oneshot::channel();
            worker
                .send_msg(TelemetryActions::CollectStats(tx))
                .await
                .unwrap();
            rx.await.unwrap();
        }

        timeout(Duration::from_secs(5), async {
            while expected_a.calls_async().await != 1 || expected_b.calls_async().await != 1 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("each owner should deliver to its own endpoint");
        assert_eq!(unexpected_a.calls_async().await, 0);
        assert_eq!(unexpected_b.calls_async().await, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn metrics_logs_replay_is_scoped_by_service() {
        const ENV: &str = "test";
        const SHARED_METRIC: &str = "shared.metric";

        let http_server = MockServer::start_async().await;
        let clients = MetricsLogsClientSet::default();
        let runtime_metadata = RuntimeMetadata::new("php", "8.3", "test");
        let instance_id = InstanceId::new("session", "runtime");

        let service_a = clients.get_or_create_metrics_logs(
            "service-a",
            ENV,
            &instance_id,
            &runtime_metadata,
            || test_config(&http_server),
            Vec::new(),
        );
        let service_b = clients.get_or_create_metrics_logs(
            "service-b",
            ENV,
            &instance_id,
            &runtime_metadata,
            || test_config(&http_server),
            Vec::new(),
        );
        for (service, unique_metric, metric_type) in [
            (
                "service-a",
                "service_a.metric",
                libdd_telemetry::data::metrics::MetricType::Count,
            ),
            (
                "service-b",
                "service_b.metric",
                libdd_telemetry::data::metrics::MetricType::Gauge,
            ),
        ] {
            for name in [SHARED_METRIC, unique_metric] {
                clients.register_metric(
                    &instance_id,
                    service,
                    ENV,
                    MetricContext {
                        name: name.to_string(),
                        tags: Vec::new(),
                        metric_type,
                        common: true,
                        namespace: libdd_telemetry::data::metrics::MetricNamespace::Tracers,
                    },
                );
            }
        }
        assert_eq!(
            clients.registered_metric_names(&instance_id, "service-a", ENV),
            HashSet::from([SHARED_METRIC.to_string(), "service_a.metric".to_string(),])
        );
        assert_eq!(
            clients.registered_metric_names(&instance_id, "service-b", ENV),
            HashSet::from([SHARED_METRIC.to_string(), "service_b.metric".to_string(),])
        );

        clients.remove_metrics_logs_client(&instance_id, "service-a", ENV, &service_a);
        clients.remove_metrics_logs_client(&instance_id, "service-b", ENV, &service_b);
        let replacement_a = clients.get_or_create_metrics_logs(
            "service-a",
            ENV,
            &instance_id,
            &runtime_metadata,
            || test_config(&http_server),
            Vec::new(),
        );
        let replacement_b = clients.get_or_create_metrics_logs(
            "service-b",
            ENV,
            &instance_id,
            &runtime_metadata,
            || test_config(&http_server),
            Vec::new(),
        );
        {
            let replacement_a = replacement_a.lock_or_panic();
            let a_metrics = &replacement_a
                .as_ref()
                .expect("service A replacement")
                .telemetry_metrics;
            assert!(a_metrics.contains_key(SHARED_METRIC));
            assert!(a_metrics.contains_key("service_a.metric"));
            assert!(!a_metrics.contains_key("service_b.metric"));
        }
        {
            let replacement_b = replacement_b.lock_or_panic();
            let b_metrics = &replacement_b
                .as_ref()
                .expect("service B replacement")
                .telemetry_metrics;
            assert!(b_metrics.contains_key(SHARED_METRIC));
            assert!(b_metrics.contains_key("service_b.metric"));
            assert!(!b_metrics.contains_key("service_a.metric"));
        }
    }
}
