// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config::AppSecConfig;
use crate::service::telemetry::InProcessTelemetryClientFactory;
use crossbeam_utils::atomic::AtomicCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use tokio::sync::OnceCell;
use tracing::{error, info};

pub type AppSecBackendFactory =
    fn(&AppSecConfig, InProcessTelemetryClientFactory) -> anyhow::Result<AppSecBackend>;

static APPSEC_BACKEND_FACTORY: OnceLock<AppSecBackendFactory> = OnceLock::new();

/// Registers the AppSec backend linked by the sidecar's embedding application.
///
/// Only the first registration in a process is retained. This allows inverting
/// the dependency between sidecar and appsec helper-rust.
pub fn register_backend_factory(factory: AppSecBackendFactory) {
    _ = APPSEC_BACKEND_FACTORY.set(factory);
}

/// Publishes one AppSec backend and coordinates its one-way lifecycle.
///
/// WARNING: This type assumes that the caller drains all sidecar connections
/// before calling [`Self::shutdown`], so initialization, message handling, and
/// disconnects do not race with shutdown (the watchdog's forced exit path
/// disconsidered).
pub(crate) struct AppSecManager {
    telemetry: InProcessTelemetryClientFactory,
    backend: OnceCell<AppSecBackendState>,
}

enum AppSecBackendState {
    Running(AppSecBackend),
    Failed,
}

impl AppSecManager {
    pub(crate) fn new(telemetry: InProcessTelemetryClientFactory) -> Self {
        Self {
            telemetry,
            backend: OnceCell::new(),
        }
    }

    pub(crate) async fn ensure_started(&self, config: &AppSecConfig) -> bool {
        matches!(
            self.backend
                .get_or_init(|| async { self.start(config) })
                .await,
            AppSecBackendState::Running(_)
        )
    }

    fn start(&self, config: &AppSecConfig) -> AppSecBackendState {
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

    pub(crate) async fn send_message(
        &self,
        session_id: &str,
        client_id: u64,
        data: Vec<u8>,
    ) -> Option<AppSecMessageResponse> {
        let AppSecBackendState::Running(backend) = self.backend.get()? else {
            return None;
        };
        Some((backend.send_message)(session_id, client_id, data).await)
    }

    pub(crate) fn disconnect(&self, session_id: &str, client_id: u64) {
        let Some(AppSecBackendState::Running(backend)) = self.backend.get() else {
            return;
        };
        (backend.disconnect)(session_id, client_id);
    }

    pub(crate) async fn shutdown(&self) {
        let Some(AppSecBackendState::Running(backend)) = self.backend.get() else {
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

type AppSecSendMessage =
    for<'a> fn(&'a str, u64, Vec<u8>) -> AppSecFuture<'a, AppSecMessageResponse>;

pub type AppSecFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type AppSecShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub struct AppSecMessageResponse {
    pub client_id: u64,
    pub data: Vec<u8>,
    pub disconnect: bool,
}

type AppSecDisconnect = fn(&str, u64);

/// A slot initialized with one value that may be taken at most once.
struct TakeSlot<T>(AtomicCell<Option<T>>);

impl<T> TakeSlot<T> {
    const fn new(value: T) -> Self {
        Self(AtomicCell::new(Some(value)))
    }

    fn take(&self) -> Option<T> {
        self.0.take()
    }
}

/// Factory result for one started helper.
pub struct AppSecBackend {
    send_message: AppSecSendMessage,
    disconnect: AppSecDisconnect,
    shutdown: TakeSlot<AppSecShutdownFuture>,
}

impl AppSecBackend {
    pub fn new(
        send_message: AppSecSendMessage,
        disconnect: AppSecDisconnect,
        shutdown: AppSecShutdownFuture,
    ) -> Self {
        Self {
            send_message,
            disconnect,
            shutdown: TakeSlot::new(shutdown),
        }
    }
}
