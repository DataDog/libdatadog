// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

// Lint removed from stable clippy after rust 1.60 - this allow can be removed once we update rust version
#![allow(clippy::needless_collect)]
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Result;

use datadog_ipc::{platform::AsyncChannel, transport::Transport};
use futures::{
    future::{self, BoxFuture, Pending, Ready, Shared},
    FutureExt,
};

use datadog_ipc::tarpc::{context::Context, server::Channel};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

use crate::{
    config::Config,
    data,
    worker::{
        store::Store, LifecycleAction, TelemetryActions, TelemetryWorkerBuilder,
        TelemetryWorkerHandle, MAX_ITEMS,
    },
};
use datadog_ipc::tarpc;

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

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
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

#[derive(Default, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Default)]
struct RuntimeInfo {
    apps: Arc<Mutex<HashMap<String, AppInstance>>>,
    enqueued_actions: Arc<Mutex<HashMap<QueueId, EnqueuedData>>>,
}

impl RuntimeInfo {
    fn get_app(&self, service_name: &String) -> Option<AppInstance> {
        let apps = self.apps.lock().unwrap();
        apps.get(service_name).map(Clone::clone)
    }

    async fn shutdown(self) {
        let instances: Vec<AppInstance> = self
            .apps
            .lock()
            .unwrap()
            .drain()
            .map(|(_, instance)| instance)
            .collect();
        let instances_shutting_down: Vec<_> = instances
            .into_iter()
            .map(|instance| {
                tokio::spawn(async move {
                    instance
                        .telemetry
                        .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop))
                        .await
                        .ok();
                    instance.telemetry_worker_shutdown.await;
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
}

#[derive(Default, Clone)]
pub struct TelemetryServer {
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
}

impl TelemetryServer {
    pub async fn accept_connection(self, socket: UnixStream) {
        let server = datadog_ipc::tarpc::server::BaseChannel::new(
            datadog_ipc::tarpc::server::Config {
                pending_response_buffer: 10000,
            },
            Transport::try_from(AsyncChannel::from(socket)).unwrap(),
        );
        datadog_ipc::sequential::execute_sequential(server.requests(), self.serve(), 100).await
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

    async fn shutdown_session(&self, session_id: &String) {
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

        if let Some(app) = rt_info.get_app(service_name) {
            return Some(app);
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
        if let Ok((handle, worker_join)) = builder.spawn_with_config(config.clone()).await {
            tracing::info!("spawning worker {config:?}");

            let instance = AppInstance {
                telemetry: handle,
                telemetry_worker_shutdown: worker_join.map(Result::ok).boxed().shared(),
            };
            rt_info
                .apps
                .lock()
                .unwrap()
                .insert(service_name.clone(), instance.clone());

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
        }
    }
}

type NoResponse = Ready<()>;

fn no_response() -> NoResponse {
    Context::current().discard_response = true;
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
        tokio::spawn(async move { TelemetryServer::shutdown_session(&self, &session_id).await });
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
        let mut queue = rt_info.enqueued_actions.lock().unwrap();
        match queue.get_mut(&queue_id) {
            Some(data) => data.process(actions),
            None => {
                queue.insert(queue_id, EnqueuedData::processed(actions));
            }
        };

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
        tokio::spawn(async move {
            let actions = self
                .get_runtime(&instance_id)
                .enqueued_actions
                .lock()
                .unwrap()
                .get_mut(&queue_id)
                .map(|data| {
                    let mut actions: Vec<TelemetryActions> = vec![];
                    for d in data.dependencies.unflushed() {
                        actions.push(TelemetryActions::AddDependecy(d.clone()));
                    }
                    for c in data.configurations.unflushed() {
                        actions.push(TelemetryActions::AddConfig(c.clone()));
                    }
                    for i in data.integrations.unflushed() {
                        actions.push(TelemetryActions::AddIntegration(i.clone()));
                    }
                    actions
                })
                .unwrap_or_default();

            if let Some(app) = self
                .get_app(&instance_id, &runtime_meta, &service_name, actions)
                .await
            {
                let actions = self
                    .get_runtime(&instance_id)
                    .enqueued_actions
                    .lock()
                    .unwrap()
                    .get_mut(&queue_id)
                    .map(|data| std::mem::take(&mut data.actions))
                    .unwrap_or_default();
                app.telemetry.send_msgs(actions).await.ok();
            }
        });

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

        Box::pin(async move { session.shutdown_running_instances().await })
    }
}

pub mod blocking {
    use std::{
        borrow::Cow,
        io,
        time::{Duration, Instant},
    };

    use datadog_ipc::transport::blocking::BlockingTransport;

    use crate::worker::TelemetryActions;

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
            queue_id: queue_id.clone(),
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
                queue_id: queue_id.clone(),
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
        let res = transport.call(TelemetryInterfaceRequest::SetSessionAgentUrl {
            session_id,
            agent_url,
        })?;
        match res {
            TelemetryInterfaceResponse::SetSessionAgentUrl(_) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::Other,
                "wrong response type when setting session agent url",
            )),
        }
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
