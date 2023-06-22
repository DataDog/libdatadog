// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

// Lint removed from stable clippy after rust 1.60 - this allow can be removed once we update rust version
#![allow(clippy::needless_collect)]
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use datadog_ipc::{platform::AsyncChannel, transport::Transport};
use futures::future::join_all;
use futures::{
    future::{self, BoxFuture, Ready, Shared},
    FutureExt,
};
use manual_future::{ManualFuture, ManualFutureCompleter};

use datadog_ipc::tarpc::{context::Context, server::Channel};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

use datadog_ipc::tarpc;
use ddtelemetry::{
    config::Config,
    data,
    worker::{
        store::Store, LifecycleAction, TelemetryActions, TelemetryWorkerBuilder,
        TelemetryWorkerHandle, MAX_ITEMS,
    },
};

#[datadog_sidecar_macros::extract_request_id]
#[tarpc::service]
pub trait TelemetryInterface {
    async fn equeue_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<TelemetryActions>,
    );
    async fn register_service_and_flush_queued_actions(
        instance_id: InstanceId,
        queue_id: QueueId,
        meta: RuntimeMeta,
        service_name: String,
    );
    async fn set_session_agent_url(session_id: String, agent_url: String);
    async fn shutdown_runtime(instance_id: InstanceId);
    async fn shutdown_session(session_id: String);
    async fn ping();
}

pub trait RequestIdentification {
    fn extract_identifier(&self) -> RequestIdentifier;
}

pub enum RequestIdentifier {
    InstanceId(InstanceId),
    SessionId(String),
    None,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeMeta {
    language_name: String,
    language_version: String,
    tracer_version: String,
}

impl RuntimeMeta {
    pub fn new<T>(language_name: T, language_version: T, tracer_version: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            language_name: language_name.into(),
            language_version: language_version.into(),
            tracer_version: tracer_version.into(),
        }
    }
}

#[derive(Default, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstanceId {
    session_id: String,
    runtime_id: String,
}

impl InstanceId {
    pub fn new<T>(session_id: T, runtime_id: T) -> Self
    where
        T: Into<String>,
    {
        InstanceId {
            session_id: session_id.into(),
            runtime_id: runtime_id.into(),
        }
    }
}

#[derive(Default, Copy, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct QueueId {
    inner: u64,
}

impl QueueId {
    pub fn new_unique() -> Self {
        Self {
            inner: rand::thread_rng().gen_range(1u64..u64::MAX),
        }
    }
}

#[derive(Default, Clone)]
struct SessionInfo {
    runtimes: Arc<Mutex<HashMap<String, RuntimeInfo>>>,
    session_config: Arc<Mutex<Option<Config>>>,
}

impl SessionInfo {
    fn get_runtime(&self, runtime_id: &String) -> RuntimeInfo {
        let mut runtimes = self.runtimes.lock().unwrap();
        match runtimes.get(runtime_id) {
            Some(runtime) => runtime.clone(),
            None => {
                let runtime = RuntimeInfo::default();
                runtimes.insert(runtime_id.clone(), runtime.clone());
                runtime
            }
        }
    }

    async fn shutdown(&self) {
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .lock()
            .unwrap()
            .drain()
            .map(|(_, instance)| instance)
            .collect();

        let runtimes_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(runtimes_shutting_down).await;
    }

    async fn shutdown_running_instances(&self) {
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, instance)| instance.clone())
            .collect();

        let instances_shutting_down: Vec<_> = runtimes
            .into_iter()
            .map(|rt| tokio::spawn(async move { rt.shutdown().await }))
            .collect();

        future::join_all(instances_shutting_down).await;
    }

    async fn shutdown_runtime(self, runtime_id: &String) {
        let runtime = match self.runtimes.lock().unwrap().remove(runtime_id) {
            Some(rt) => rt,
            None => return,
        };

        runtime.shutdown().await
    }

    fn get_config(&self) -> MutexGuard<Option<Config>> {
        let mut cfg = self.session_config.lock().unwrap();

        if (*cfg).is_none() {
            *cfg = Some(Config::from_env())
        }

        cfg
    }

    fn modify_config<F>(&self, mut f: F)
    where
        F: FnMut(&mut Config),
    {
        if let Some(cfg) = &mut *self.get_config() {
            f(cfg)
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum AppOrQueue {
    App(Shared<ManualFuture<String>>),
    Queue(EnqueuedData),
}

#[allow(clippy::type_complexity)]
#[derive(Clone, Default)]
struct RuntimeInfo {
    apps: Arc<Mutex<HashMap<String, Shared<ManualFuture<Option<AppInstance>>>>>>,
    app_or_actions: Arc<Mutex<HashMap<QueueId, AppOrQueue>>>,
}

impl RuntimeInfo {
    #[allow(clippy::type_complexity)]
    fn get_app(
        &self,
        service_name: &String,
    ) -> (
        Shared<ManualFuture<Option<AppInstance>>>,
        Option<ManualFutureCompleter<Option<AppInstance>>>,
    ) {
        let mut apps = self.apps.lock().unwrap();
        if let Some(found) = apps.get(service_name) {
            (found.clone(), None)
        } else {
            let (future, completer) = ManualFuture::new();
            let shared = future.shared();
            apps.insert(service_name.clone(), shared.clone());
            (shared, Some(completer))
        }
    }

    async fn shutdown(self) {
        let instance_futures: Vec<_> = self
            .apps
            .lock()
            .unwrap()
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
    }
}

#[derive(Clone)]
struct AppInstance {
    telemetry: TelemetryWorkerHandle,
    telemetry_worker_shutdown: Shared<BoxFuture<'static, Option<()>>>,
}

struct EnqueuedData {
    dependencies: Store<data::Dependency>,
    configurations: Store<data::Configuration>,
    integrations: Store<data::Integration>,
    actions: Vec<TelemetryActions>,
}

impl Default for EnqueuedData {
    fn default() -> Self {
        Self {
            dependencies: Store::new(MAX_ITEMS),
            configurations: Store::new(MAX_ITEMS),
            integrations: Store::new(MAX_ITEMS),
            actions: Vec::new(),
        }
    }
}

impl EnqueuedData {
    pub fn process(&mut self, actions: Vec<TelemetryActions>) {
        for action in actions {
            match action {
                TelemetryActions::AddConfig(c) => self.configurations.insert(c),
                TelemetryActions::AddDependecy(d) => self.dependencies.insert(d),
                TelemetryActions::AddIntegration(i) => self.integrations.insert(i),
                other => self.actions.push(other),
            }
        }
    }

    pub fn processed(action: Vec<TelemetryActions>) -> Self {
        let mut data = Self::default();
        data.process(action);
        data
    }

    fn extract_telemetry_actions(&mut self, actions: &mut Vec<TelemetryActions>) {
        for d in self.dependencies.unflushed() {
            actions.push(TelemetryActions::AddDependecy(d.clone()));
        }
        for c in self.configurations.unflushed() {
            actions.push(TelemetryActions::AddConfig(c.clone()));
        }
        for i in self.integrations.unflushed() {
            actions.push(TelemetryActions::AddIntegration(i.clone()));
        }
    }
}

#[derive(Default, Clone)]
pub struct TelemetryServer {
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    session_counter: Arc<Mutex<HashMap<String, u32>>>,
    pub self_telemetry_config: Arc<Mutex<Option<ManualFutureCompleter<Config>>>>,
    pub submitted_payloads: Arc<AtomicU64>,
}

impl TelemetryServer {
    pub async fn accept_connection(self, socket: UnixStream) {
        let server = datadog_ipc::tarpc::server::BaseChannel::new(
            datadog_ipc::tarpc::server::Config {
                pending_response_buffer: 10000,
            },
            Transport::try_from(AsyncChannel::from(socket)).unwrap(),
        );

        let mut executor = datadog_ipc::sequential::execute_sequential(
            server.requests(),
            self.clone().serve(),
            100,
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel::<_>(100);
        let tx = executor.swap_sender(tx);

        let session_counter = self.session_counter.clone();
        let submitted_payloads = self.submitted_payloads.clone();
        let session_interceptor = tokio::spawn(async move {
            let mut sessions = HashSet::new();
            let mut instances = HashSet::new();
            loop {
                let (serve, req) = match rx.recv().await {
                    None => return (sessions, instances),
                    Some(s) => s,
                };

                submitted_payloads.fetch_add(1, Ordering::SeqCst);

                let instance: RequestIdentifier = req.get().extract_identifier();
                if tx.send((serve, req)).await.is_ok() {
                    if let RequestIdentifier::InstanceId(ref instance_id) = instance {
                        instances.insert(instance_id.clone());
                    }
                    if let RequestIdentifier::SessionId(session)
                    | RequestIdentifier::InstanceId(InstanceId {
                        session_id: session,
                        ..
                    }) = instance
                    {
                        if sessions.insert(session.clone()) {
                            match session_counter.lock().unwrap().entry(session) {
                                Entry::Occupied(mut entry) => entry.insert(entry.get() + 1),
                                Entry::Vacant(entry) => *entry.insert(1),
                            };
                        }
                    }
                }
            }
        });

        executor.await;
        if let Ok((sessions, instances)) = session_interceptor.await {
            for session in sessions {
                let stop = {
                    let mut counter = self.session_counter.lock().unwrap();
                    if let Entry::Occupied(mut entry) = counter.entry(session.clone()) {
                        if entry.insert(entry.get() - 1) == 1 {
                            entry.remove();
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if stop {
                    self.stop_session(&session).await;
                }
            }
            for instance_id in instances {
                let maybe_session = self
                    .sessions
                    .lock()
                    .unwrap()
                    .get(&instance_id.session_id)
                    .cloned();
                if let Some(session) = maybe_session {
                    session.shutdown_runtime(&instance_id.runtime_id).await;
                }
            }
        }
    }

    pub fn active_session_count(&self) -> usize {
        self.session_counter.lock().unwrap().len()
    }

    fn get_session(&self, session_id: &String) -> SessionInfo {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(session_id) {
            Some(session) => session.clone(),
            None => {
                let session = SessionInfo::default();
                sessions.insert(session_id.clone(), session.clone());
                session
            }
        }
    }

    fn get_runtime(&self, instance_id: &InstanceId) -> RuntimeInfo {
        let session = self.get_session(&instance_id.session_id);
        session.get_runtime(&instance_id.runtime_id)
    }

    async fn stop_session(&self, session_id: &String) {
        let session = match self.sessions.lock().unwrap().remove(session_id) {
            Some(session) => session,
            None => return,
        };

        session.shutdown().await
    }

    async fn get_app(
        &self,
        instance_id: &InstanceId,
        runtime_meta: &RuntimeMeta,
        service_name: &String,
        inital_actions: Vec<TelemetryActions>,
    ) -> Option<AppInstance> {
        let rt_info = self.get_runtime(instance_id);

        let (app_future, completer) = rt_info.get_app(service_name);
        if completer.is_none() {
            return app_future.await;
        }

        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            service_name.clone(),
            runtime_meta.language_name.clone(),
            runtime_meta.language_version.clone(),
            runtime_meta.tracer_version.clone(),
        );
        builder.runtime_id = Some(instance_id.runtime_id.clone());

        let session_info = self.get_session(&instance_id.session_id);
        let config = session_info
            .session_config
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(Config::from_env);

        // TODO: log errors
        let instance_option =
            if let Ok((handle, worker_join)) = builder.spawn_with_config(config.clone()).await {
                tracing::info!("spawning worker {config:?}");

                let instance = AppInstance {
                    telemetry: handle,
                    telemetry_worker_shutdown: worker_join.map(Result::ok).boxed().shared(),
                };

                instance
                    .telemetry
                    .send_msgs(inital_actions.into_iter())
                    .await
                    .ok();

                instance
                    .telemetry
                    .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
                    .await
                    .ok();
                Some(instance)
            } else {
                None
            };
        completer.unwrap().complete(instance_option).await;
        app_future.await
    }
}

type NoResponse = Ready<()>;

fn no_response() -> NoResponse {
    future::ready(())
}

impl TelemetryInterface for TelemetryServer {
    type PingFut = Ready<()>;

    fn ping(self, _: Context) -> Self::PingFut {
        future::ready(())
    }

    type ShutdownRuntimeFut = NoResponse;
    fn shutdown_runtime(self, _: Context, instance_id: InstanceId) -> Self::ShutdownRuntimeFut {
        let session = self.get_session(&instance_id.session_id);
        tokio::spawn(async move { session.shutdown_runtime(&instance_id.runtime_id).await });

        no_response()
    }

    type ShutdownSessionFut = NoResponse;

    fn shutdown_session(self, _: Context, session_id: String) -> Self::ShutdownSessionFut {
        tokio::spawn(async move { TelemetryServer::stop_session(&self, &session_id).await });
        no_response()
    }

    type EqueueActionsFut = NoResponse;

    fn equeue_actions(
        self,
        _context: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<TelemetryActions>,
    ) -> Self::EqueueActionsFut {
        let rt_info = self.get_runtime(&instance_id);
        let mut queue = rt_info.app_or_actions.lock().unwrap();
        match queue.entry(queue_id) {
            Entry::Occupied(mut entry) => match entry.get_mut() {
                AppOrQueue::Queue(ref mut data) => {
                    data.process(actions);
                }
                AppOrQueue::App(service_future) => {
                    let service_future = service_future.clone();
                    // drop on stop
                    if actions.iter().any(|action| {
                        matches!(action, TelemetryActions::Lifecycle(LifecycleAction::Stop))
                    }) {
                        entry.remove();
                    }
                    let apps = rt_info.apps.clone();
                    tokio::spawn(async move {
                        let service = service_future.await;
                        let app_future = if let Some(fut) = apps.lock().unwrap().get(&service) {
                            fut.clone()
                        } else {
                            return;
                        };
                        if let Some(app) = app_future.await {
                            app.telemetry.send_msgs(actions).await.ok();
                        }
                    });
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(AppOrQueue::Queue(EnqueuedData::processed(actions)));
            }
        }

        no_response()
    }

    type RegisterServiceAndFlushQueuedActionsFut = NoResponse;

    fn register_service_and_flush_queued_actions(
        self,
        _: Context,
        instance_id: InstanceId,
        queue_id: QueueId,
        runtime_meta: RuntimeMeta,
        service_name: String,
    ) -> Self::RegisterServiceAndFlushQueuedActionsFut {
        // We need a channel to have enqueuing code await
        let (future, completer) = ManualFuture::new();
        let app_or_queue = {
            let rt_info = self.get_runtime(&instance_id);
            let mut app_or_actions = rt_info.app_or_actions.lock().unwrap();
            match app_or_actions.get(&queue_id) {
                Some(AppOrQueue::Queue(_)) => {
                    app_or_actions.insert(queue_id, AppOrQueue::App(future.shared()))
                }
                None => Some(AppOrQueue::Queue(EnqueuedData::default())),
                _ => None,
            }
        };
        if let Some(AppOrQueue::Queue(mut enqueued_data)) = app_or_queue {
            let mut actions: Vec<TelemetryActions> = vec![];
            enqueued_data.extract_telemetry_actions(&mut actions);

            tokio::spawn(async move {
                if let Some(app) = self
                    .get_app(&instance_id, &runtime_meta, &service_name, actions)
                    .await
                {
                    let actions: Vec<_> = std::mem::take(&mut enqueued_data.actions);
                    // drop on stop
                    if actions.iter().any(|action| {
                        matches!(action, TelemetryActions::Lifecycle(LifecycleAction::Stop))
                    }) {
                        self.get_runtime(&instance_id)
                            .app_or_actions
                            .lock()
                            .unwrap()
                            .remove(&queue_id);
                    }

                    app.telemetry.send_msgs(actions).await.ok();
                    // Ok, we dequeued all messages, now new enqueue_actions calls can handle it
                    completer.complete(service_name).await;
                }
            });
        }

        no_response()
    }

    type SetSessionAgentUrlFut = Pin<Box<dyn Send + futures::Future<Output = ()>>>;

    fn set_session_agent_url(
        self,
        _: Context,
        session_id: String,
        agent_url: String,
    ) -> Self::SetSessionAgentUrlFut {
        let session = self.get_session(&session_id);
        session.modify_config(|cfg| {
            cfg.set_url(&agent_url).ok();
        });

        if let Some(completer) = self.self_telemetry_config.lock().unwrap().take() {
            let config = session.session_config.lock().unwrap().as_ref().unwrap().clone();
            tokio::spawn(async move {
                completer.complete(config).await;
            });
        }

        Box::pin(async move {
            session.shutdown_running_instances().await;
            no_response().await
        })
    }
}

pub mod blocking {
    use std::{
        borrow::Cow,
        io,
        time::{Duration, Instant},
    };

    use datadog_ipc::transport::blocking::BlockingTransport;

    use ddtelemetry::worker::TelemetryActions;

    use super::{
        InstanceId, QueueId, RuntimeMeta, TelemetryInterfaceRequest, TelemetryInterfaceResponse,
    };

    pub type TelemetryTransport =
        BlockingTransport<TelemetryInterfaceResponse, TelemetryInterfaceRequest>;

    pub fn shutdown_runtime(
        transport: &mut TelemetryTransport,
        instance_id: &InstanceId,
    ) -> io::Result<()> {
        transport.send(TelemetryInterfaceRequest::ShutdownRuntime {
            instance_id: instance_id.clone(),
        })
    }

    pub fn shutdown_session(
        transport: &mut TelemetryTransport,
        session_id: String,
    ) -> io::Result<()> {
        transport.send(TelemetryInterfaceRequest::ShutdownSession { session_id })
    }

    pub fn enqueue_actions(
        transport: &mut TelemetryTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        actions: Vec<TelemetryActions>,
    ) -> io::Result<()> {
        transport.send(TelemetryInterfaceRequest::EqueueActions {
            instance_id: instance_id.clone(),
            queue_id: *queue_id,
            actions,
        })
    }

    pub fn register_service_and_flush_queued_actions(
        transport: &mut TelemetryTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        runtime_metadata: &RuntimeMeta,
        service_name: Cow<str>,
    ) -> io::Result<()> {
        transport.send(
            TelemetryInterfaceRequest::RegisterServiceAndFlushQueuedActions {
                instance_id: instance_id.clone(),
                queue_id: *queue_id,
                meta: runtime_metadata.clone(),
                service_name: service_name.into_owned(),
            },
        )
    }

    pub fn set_session_agent_url(
        transport: &mut TelemetryTransport,
        session_id: String,
        agent_url: String,
    ) -> io::Result<()> {
        transport.send(TelemetryInterfaceRequest::SetSessionAgentUrl {
            session_id,
            agent_url,
        })
    }

    pub fn ping(transport: &mut TelemetryTransport) -> io::Result<Duration> {
        let start = Instant::now();
        transport.call(TelemetryInterfaceRequest::Ping {})?;

        Ok(Instant::now()
            .checked_duration_since(start)
            .unwrap_or_default())
    }
}

mod transfer_handles_impl {

    use datadog_ipc::handles::{HandlesTransport, TransferHandles};

    use super::{TelemetryInterfaceRequest, TelemetryInterfaceResponse};

    impl TransferHandles for TelemetryInterfaceResponse {
        fn move_handles<Transport: HandlesTransport>(
            &self,
            _transport: Transport,
        ) -> Result<(), Transport::Error> {
            Ok(())
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            _transport: Transport,
        ) -> Result<(), Transport::Error> {
            Ok(())
        }
    }

    impl TransferHandles for TelemetryInterfaceRequest {
        fn move_handles<Transport: HandlesTransport>(
            &self,
            _transport: Transport,
        ) -> Result<(), Transport::Error> {
            Ok(())
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            _transport: Transport,
        ) -> Result<(), Transport::Error> {
            Ok(())
        }
    }
}
