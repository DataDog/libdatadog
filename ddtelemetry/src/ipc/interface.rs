use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use futures::Future;
use futures::{
    future::{self, BoxFuture, Pending, Ready, Shared},
    FutureExt,
};
use tarpc::server::Channel;
use tokio::net::UnixStream;

use crate::worker::{TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle};

use super::{platform::AsyncChannel, transport::Transport};

#[tarpc::service]
pub trait TelemetryInterface {
    async fn register_application(
        session_id: String,
        runtime_id: String,
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> ();
    async fn send_telemetry_actions(
        session_id: String,
        runtime_id: String,
        service_name: String,
        actions: Vec<TelemetryActions>,
    ) -> ();
    async fn shutdown_runtime(session_id: String, runtime_id: String) -> ();
    async fn shutdown_session(session_id: String) -> ();
    async fn ping() -> ();
}

#[derive(Default, Clone)]
struct SessionInfo {
    runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
}

impl SessionInfo {
    fn get_runtime(&self, runtime_id: &String) -> Runtime {
        let mut runtimes = self.runtimes.lock().unwrap();
        match runtimes.get(runtime_id) {
            Some(runtime) => runtime.clone(),
            None => {
                let runtime = Runtime::default();
                runtimes.insert(runtime_id.clone(), runtime.clone());
                runtime
            }
        }
    }
    async fn shutdown(&self) {
        let runtimes: Vec<Runtime> = self
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

    async fn shutdown_runtime(self, runtime_id: &String) {
        let runtime = match self.runtimes.lock().unwrap().remove(runtime_id) {
            Some(rt) => rt,
            None => return,
        };

        runtime.shutdown().await
    }
}

#[derive(Clone, Default)]
struct Runtime {
    apps: Arc<Mutex<HashMap<String, AppInstance>>>,
}

impl Runtime {
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
                        .send_msg(crate::worker::TelemetryActions::Stop)
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

#[derive(Default, Clone)]
pub struct TelemetryServer {
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
}

impl TelemetryServer {
    pub async fn accept_connection(self, socket: UnixStream) {
        let server = tarpc::server::BaseChannel::new(
            tarpc::server::Config {
                pending_response_buffer: 100000,
            },
            Transport::try_from(AsyncChannel::from(socket)).unwrap(),
        );

        server.execute(self.serve()).await
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
    async fn shutdown_session(&self, session_id: &String) {
        let session = match self.sessions.lock().unwrap().remove(session_id) {
            Some(session) => session,
            None => return,
        };

        session.shutdown().await
    }
}

impl TelemetryInterface for TelemetryServer {
    /// Returning Pending future, makes the RPC send no response back
    type RegisterApplicationFut = Pending<()>;

    fn register_application(
        self,
        _: tarpc::context::Context,
        runtime_id: String,
        session_id: String,
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> Self::RegisterApplicationFut {
        tokio::spawn(async move {
            let info = self.get_session(&session_id).get_runtime(&runtime_id);

            if info.get_app(&service_name).is_some() {
                return;
            }

            let mut builder = TelemetryWorkerBuilder::new_fetch_host(
                service_name.clone(),
                language_name,
                language_version,
                tracer_version,
            );
            builder.runtime_id = Some(runtime_id.clone());

            // TODO: log errors
            if let Ok((handle, worker_join)) = builder.spawn().await {
                info.apps.lock().unwrap().insert(
                    service_name,
                    AppInstance {
                        telemetry: handle,
                        telemetry_worker_shutdown: worker_join.map(Result::ok).boxed().shared(),
                    },
                );
            };
        });

        future::pending()
    }

    type PingFut = Ready<()>;

    fn ping(self, _: tarpc::context::Context) -> Self::PingFut {
        future::ready(())
    }

    type ShutdownRuntimeFut = Pending<()>;
    fn shutdown_runtime(
        self,
        _: tarpc::context::Context,
        session_id: String,
        runtime_id: String,
    ) -> Self::ShutdownRuntimeFut {
        let session = self.get_session(&session_id);
        tokio::spawn(async move { session.shutdown_runtime(&runtime_id).await });

        future::pending()
    }

    type ShutdownSessionFut = Pending<()>;

    fn shutdown_session(
        self,
        _: tarpc::context::Context,
        session_id: String,
    ) -> Self::ShutdownSessionFut {
        tokio::spawn(async move { TelemetryServer::shutdown_session(&self, &session_id).await });
        future::pending()
    }

    type SendTelemetryActionsFut = Pending<()>;

    fn send_telemetry_actions(
        self,
        _: tarpc::context::Context,
        session_id: String,
        runtime_id: String,
        service_name: String,
        actions: Vec<TelemetryActions>,
    ) -> Self::SendTelemetryActionsFut {
        let app = match self
            .get_session(&session_id)
            .get_runtime(&runtime_id)
            .get_app(&service_name)
        {
            Some(app) => app,
            None => return future::pending(),
        };

        tokio::spawn(async move {
            for action in actions {
                app.telemetry.send_msg(action).await.ok();
            }
        });

        future::pending()
    }
}

pub mod blocking {
    use std::{
        io,
        time::{Duration, Instant},
    };

    use crate::{ipc::transport::blocking::BlockingTransport, worker::TelemetryActions};

    use super::{TelemetryInterfaceRequest, TelemetryInterfaceResponse};

    pub type TelemetryTransport =
        BlockingTransport<TelemetryInterfaceResponse, TelemetryInterfaceRequest>;

    pub fn register_application(
        transport: &mut TelemetryTransport,
        runtime_id: String,
        session_id: String,
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> io::Result<()> {
        transport.send_ignore_response(TelemetryInterfaceRequest::RegisterApplication {
            runtime_id,
            session_id,
            service_name,
            language_name,
            language_version,
            tracer_version,
        })
    }

    pub fn shutdown_runtime(
        transport: &mut TelemetryTransport,
        runtime_id: String,
        session_id: String,
    ) -> io::Result<()> {
        transport.send_ignore_response(TelemetryInterfaceRequest::ShutdownRuntime {
            session_id,
            runtime_id,
        })
    }

    pub fn shutdown_session(
        transport: &mut TelemetryTransport,
        session_id: String,
    ) -> io::Result<()> {
        transport.send_ignore_response(TelemetryInterfaceRequest::ShutdownSession { session_id })
    }

    pub fn send_telemetry_actions(
        transport: &mut TelemetryTransport,
        runtime_id: String,
        session_id: String,
        service_name: String,
        actions: Vec<TelemetryActions>,
    ) -> io::Result<()> {
        transport.send_ignore_response(TelemetryInterfaceRequest::SendTelemetryActions {
            session_id,
            runtime_id,
            service_name,
            actions,
        })
    }

    pub fn ping(transport: &mut TelemetryTransport) -> io::Result<Duration> {
        let start = Instant::now();
        transport.send(TelemetryInterfaceRequest::Ping {})?;

        Ok(Instant::now()
            .checked_duration_since(start)
            .unwrap_or_default())
    }
}

mod transfer_handles_impl {
    use crate::ipc::handles::{HandlesTransport, TransferHandles};

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
