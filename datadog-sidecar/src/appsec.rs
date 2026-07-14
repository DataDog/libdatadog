// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config::AppSecConfig;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use tracing::{error, info};

pub type AppSecBackendFactory = fn(&AppSecConfig) -> anyhow::Result<AppSecBackend>;

static APPSEC_BACKEND_FACTORY: OnceLock<AppSecBackendFactory> = OnceLock::new();

/// Registers the AppSec backend linked by the sidecar's embedding application.
///
/// Only the first registration in a process is retained. This allows inverting
/// the dependency between sidecar and appsec helper-rust.
pub fn register_backend_factory(factory: AppSecBackendFactory) {
    _ = APPSEC_BACKEND_FACTORY.set(factory);
}
pub struct AppSec {
    backend: AppSecBackend,
}

impl AppSec {
    pub fn start(config: &AppSecConfig) -> Option<Self> {
        info!("Starting appsec backend");

        let Some(factory) = APPSEC_BACKEND_FACTORY.get() else {
            error!("No appsec backend is registered");
            return None;
        };

        let backend = match factory(config) {
            Ok(backend) => backend,
            Err(err) => {
                error!("Appsec backend failed to start: {err:#}");
                return None;
            }
        };

        info!("Appsec backend started");
        Some(Self { backend })
    }

    pub(crate) fn backend(&self) -> AppSecBackendCallbacks {
        self.backend.callbacks
    }

    pub async fn shutdown(self) {
        info!("Shutting down appsec backend");
        self.backend.shutdown.await;
        info!("Appsec backend shutdown");
    }
}

type AppSecSendMessage =
    for<'a> fn(&'a str, u64, Vec<u8>) -> AppSecFuture<'a, AppSecMessageResponse>;

pub type AppSecFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub struct AppSecMessageResponse {
    pub client_id: u64,
    pub data: Vec<u8>,
    pub disconnect: bool,
}

type AppSecDisconnect = fn(&str, u64);

#[derive(Clone, Copy)]
pub(crate) struct AppSecBackendCallbacks {
    send_message: AppSecSendMessage,
    disconnect: AppSecDisconnect,
}

impl AppSecBackendCallbacks {
    pub(crate) fn send_message<'a>(
        &self,
        session_id: &'a str,
        client_id: u64,
        data: Vec<u8>,
    ) -> AppSecFuture<'a, AppSecMessageResponse> {
        (self.send_message)(session_id, client_id, data)
    }

    /// If client_id is 0, this is a session-wide disconnect.
    pub(crate) fn disconnect(&self, session_id: &str, client_id: u64) {
        (self.disconnect)(session_id, client_id);
    }
}

pub struct AppSecBackend {
    callbacks: AppSecBackendCallbacks,
    shutdown: AppSecFuture<'static, ()>,
}

impl AppSecBackend {
    pub fn new(
        send_message: AppSecSendMessage,
        disconnect: AppSecDisconnect,
        shutdown: AppSecFuture<'static, ()>,
    ) -> Self {
        Self {
            callbacks: AppSecBackendCallbacks {
                send_message,
                disconnect,
            },
            shutdown,
        }
    }
}
