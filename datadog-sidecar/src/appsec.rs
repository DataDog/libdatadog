// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Hook for an AppSec backend supplied by the embedding application.
//!
//! The sidecar owns the lifecycle of the AppSec client of each connection: the client is
//! configured with the session config, follows the connection's application, handles the
//! connection's requests and is dropped with the connection. The backend and its protocol are not
//! part of libdatadog.

use crate::service::ConnectionSessionHandle;
use crate::service::telemetry::InProcessTelemetryClientFactory;
use crossbeam_utils::atomic::AtomicCell;
use std::any::Any;
use std::ffi::CString;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
#[cfg(unix)]
use tokio::sync::OnceCell;
#[cfg(unix)]
use tracing::{error, info};

/// The AppSec protocol types.
///
/// With the `appsec-protocol` feature, the embedding application provides them in the file
/// named by `DATADOG_SIDECAR_APPSEC_PROTOCOL`, which is included here. Otherwise, a placeholder
/// is used. The file must define, using only `serde` and `std`:
/// - `AppSecSessionConfig: Clone + PartialEq + Debug`, sent with the session config;
/// - `AppSecRequest: Debug`;
/// - `AppSecVerdict: Default + Debug`, whose default answers requests no backend can handle;
///
/// all of them `Serialize + DeserializeOwned + Send + Sync + 'static`.
#[cfg(appsec_protocol_external)]
pub mod protocol {
    include!(env!("DATADOG_SIDECAR_APPSEC_PROTOCOL"));
}
#[cfg(not(appsec_protocol_external))]
#[path = "appsec_protocol_placeholder.rs"]
pub mod protocol;

use protocol::{AppSecRequest, AppSecSessionConfig, AppSecVerdict};

/// Starts the backend, with the AppSec config of the first session that has one.
pub type AppSecBackendFactory =
    fn(&AppSecSessionConfig, InProcessTelemetryClientFactory) -> anyhow::Result<AppSecBackend>;

static APPSEC_BACKEND_FACTORY: OnceLock<AppSecBackendFactory> = OnceLock::new();

/// Registers the AppSec backend linked by the sidecar's embedding application.
///
/// Only the first registration in a process is retained. This allows inverting
/// the dependency between sidecar and appsec helper-rust.
pub fn register_backend_factory(factory: AppSecBackendFactory) {
    _ = APPSEC_BACKEND_FACTORY.set(factory);
}

/// The application the requests of a connection are currently attributed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSecApplication {
    pub service: String,
    pub env: String,
    pub version: String,
    /// The shared memory the application's remote configs are published in, if remote config is
    /// enabled.
    pub remote_config_path: Option<CString>,
}

/// The AppSec side of one sidecar connection. A connection serves one extension thread, which
/// processes one request at a time, so it talks to one backend client at a time: the connection
/// identifies the client.
pub struct AppSecConnection {
    /// The session of the connection, for the client to submit telemetry with.
    pub session: ConnectionSessionHandle,
    /// The backend's client of this connection. Only the backend sets and reads it.
    pub client: Option<Box<dyn Any + Send + Sync>>,
    /// The application last passed to the backend.
    #[cfg(unix)]
    application: Option<AppSecApplication>,
}

impl AppSecConnection {
    pub fn new(session: ConnectionSessionHandle) -> Self {
        AppSecConnection {
            session,
            client: None,
            #[cfg(unix)]
            application: None,
        }
    }
}

/// Publishes one AppSec backend and coordinates its one-way lifecycle. The backend is
/// unix-only: elsewhere, requests get the protocol's default verdict.
///
/// WARNING: This type assumes that the caller drains all sidecar connections
/// before calling [`Self::shutdown`], so initialization, message handling, and
/// disconnects do not race with shutdown (the watchdog's forced exit path
/// disconsidered).
#[cfg(unix)]
pub(crate) struct AppSecManager {
    telemetry: InProcessTelemetryClientFactory,
    backend: OnceCell<AppSecBackendState>,
}

#[cfg(unix)]
enum AppSecBackendState {
    Running(AppSecBackend),
    Failed,
}

#[cfg(unix)]
impl AppSecManager {
    pub(crate) fn new(telemetry: InProcessTelemetryClientFactory) -> Self {
        Self {
            telemetry,
            backend: OnceCell::new(),
        }
    }

    fn running(&self) -> Option<&AppSecBackend> {
        match self.backend.get()? {
            AppSecBackendState::Running(backend) => Some(backend),
            AppSecBackendState::Failed => None,
        }
    }

    async fn ensure_started(&self, config: &AppSecSessionConfig) -> Option<&AppSecBackend> {
        match self
            .backend
            .get_or_init(|| async { self.start(config) })
            .await
        {
            AppSecBackendState::Running(backend) => Some(backend),
            AppSecBackendState::Failed => None,
        }
    }

    fn start(&self, config: &AppSecSessionConfig) -> AppSecBackendState {
        info!("Starting appsec backend");

        let Some(factory) = APPSEC_BACKEND_FACTORY.get() else {
            error!("No appsec backend is registered");
            return AppSecBackendState::Failed;
        };

        match factory(config, self.telemetry.clone()) {
            Ok(backend) => {
                info!("Appsec backend started");
                AppSecBackendState::Running(backend)
            }
            Err(err) => {
                error!("Appsec backend failed to start: {err:#}");
                AppSecBackendState::Failed
            }
        }
    }

    /// The connection got a new session config. A session without AppSec config has no client.
    pub(crate) async fn set_session_config(
        &self,
        connection: &mut AppSecConnection,
        config: Option<&AppSecSessionConfig>,
    ) {
        // The application is registered for the previous session: the client sends it again.
        connection.application = None;
        match config {
            Some(config) => {
                if let Some(backend) = self.ensure_started(config).await {
                    (backend.callbacks.on_session_config)(connection, config);
                }
            }
            None => self.disconnect(connection),
        }
    }

    /// The connection's application, once it is registered for remote config. Unchanged
    /// applications are not passed on.
    pub(crate) fn set_application(
        &self,
        connection: &mut AppSecConnection,
        application: AppSecApplication,
    ) {
        if connection.application.as_ref() == Some(&application) {
            return;
        }
        if let Some(backend) = self.running() {
            (backend.callbacks.on_application)(connection, &application);
        }
        connection.application = Some(application);
    }

    pub(crate) async fn request(
        &self,
        connection: &mut AppSecConnection,
        request: AppSecRequest,
    ) -> AppSecVerdict {
        let Some(backend) = self.running() else {
            return AppSecVerdict::default();
        };
        (backend.callbacks.on_request)(connection, request).await
    }

    pub(crate) fn disconnect(&self, connection: &mut AppSecConnection) {
        if let Some(backend) = self.running() {
            (backend.callbacks.on_disconnect)(connection);
        }
    }

    pub(crate) async fn shutdown(&self) {
        let Some(backend) = self.running() else {
            return;
        };
        let Some(shutdown) = backend.shutdown.take() else {
            return;
        };

        info!("Shutting down appsec backend");
        shutdown.await;
        info!("Appsec backend shutdown");
    }
}

pub type AppSecFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type AppSecShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// How the sidecar drives the backend's client of a connection. The calls of one connection
/// never overlap, and arrive in the order the connection's messages do.
pub struct AppSecCallbacks {
    /// The connection got a session with this AppSec config: create the client, or adapt it.
    pub on_session_config: fn(&mut AppSecConnection, &AppSecSessionConfig),
    /// The connection's requests are now attributed to this application. Must not block.
    pub on_application: fn(&mut AppSecConnection, &AppSecApplication),
    /// Handles a request of the connection.
    pub on_request:
        for<'a> fn(&'a mut AppSecConnection, AppSecRequest) -> AppSecFuture<'a, AppSecVerdict>,
    /// The connection is closed, or its session has no AppSec config anymore: drop the client.
    pub on_disconnect: fn(&mut AppSecConnection),
}

/// A slot initialized with one value that may be taken at most once.
struct TakeSlot<T>(AtomicCell<Option<T>>);

impl<T> TakeSlot<T> {
    const fn new(value: T) -> Self {
        Self(AtomicCell::new(Some(value)))
    }

    #[cfg(unix)]
    fn take(&self) -> Option<T> {
        self.0.take()
    }
}

/// Factory result for one started backend.
#[cfg_attr(not(unix), allow(dead_code))]
pub struct AppSecBackend {
    callbacks: AppSecCallbacks,
    shutdown: TakeSlot<AppSecShutdownFuture>,
}

impl AppSecBackend {
    pub fn new(callbacks: AppSecCallbacks, shutdown: AppSecShutdownFuture) -> Self {
        Self {
            callbacks,
            shutdown: TakeSlot::new(shutdown),
        }
    }
}
