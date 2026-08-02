// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config::AppSecConfig;
use crate::service::telemetry::InProcessTelemetryClientFactory;
use crossbeam_utils::atomic::AtomicCell;
use std::cell::UnsafeCell;
use std::future::Future;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use tokio::sync::Notify;
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
pub(crate) struct AppSecManager {
    telemetry: InProcessTelemetryClientFactory,
    phase: AtomicCell<AppSecPhase>,
    initialization_finished: Notify,
    // Calling send_message/disconnect is allowed to race with shutdown. It either
    // completes successfully or observes a closed channel in the backend and fails,
    // returning a message requesting a disconnect.
    send_message: OnceLock<AppSecSendMessage>,
    disconnect: OnceLock<AppSecDisconnect>,
    shutdown: TakeSlot<AppSecShutdownFuture>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum AppSecPhase {
    Uninitialized,
    Starting,
    Running,
    ShuttingDown,
    Stopped,
}

const _: () = assert!(AtomicCell::<AppSecPhase>::is_lock_free());

impl AppSecManager {
    pub(crate) fn new(telemetry: InProcessTelemetryClientFactory) -> Self {
        Self {
            telemetry,
            phase: AtomicCell::new(AppSecPhase::Uninitialized),
            initialization_finished: Notify::new(),
            send_message: OnceLock::new(),
            disconnect: OnceLock::new(),
            shutdown: TakeSlot::new(),
        }
    }

    pub(crate) async fn ensure_started(&self, config: &AppSecConfig) -> bool {
        loop {
            match self.phase() {
                AppSecPhase::Uninitialized => {
                    if self.transition(AppSecPhase::Uninitialized, AppSecPhase::Starting) {
                        return self.start(config);
                    }
                }
                AppSecPhase::Starting => {
                    self.wait_for_initialization().await;
                    return self.phase() == AppSecPhase::Running;
                }
                AppSecPhase::Running => return true,
                AppSecPhase::ShuttingDown | AppSecPhase::Stopped => return false,
            }
        }
    }

    fn start(&self, config: &AppSecConfig) -> bool {
        info!("Starting appsec backend");

        let Some(factory) = APPSEC_BACKEND_FACTORY.get() else {
            error!("No appsec backend is registered");
            self.finish_initialization(AppSecPhase::Stopped);
            return false;
        };

        let backend = match factory(config, self.telemetry.clone()) {
            Ok(backend) => backend,
            Err(err) => {
                error!("Appsec backend failed to start: {err:#}");
                self.finish_initialization(AppSecPhase::Stopped);
                return false;
            }
        };

        let AppSecBackend {
            send_message,
            disconnect,
            shutdown,
        } = backend;
        if self.send_message.set(send_message).is_err()
            || self.disconnect.set(disconnect).is_err()
            || self.shutdown.set(shutdown).is_err()
        {
            error!("AppSec backend was already published");
            self.finish_initialization(AppSecPhase::Stopped);
            return false;
        }

        self.finish_initialization(AppSecPhase::Running);
        info!("Appsec backend started");
        true
    }

    pub(crate) async fn send_message(
        &self,
        session_id: &str,
        client_id: u64,
        data: Vec<u8>,
    ) -> Option<AppSecMessageResponse> {
        if self.phase() != AppSecPhase::Running {
            return None;
        }
        let send_message = *self.send_message.get()?;
        Some(send_message(session_id, client_id, data).await)
    }

    pub(crate) fn disconnect(&self, session_id: &str, client_id: u64) {
        if self.phase() != AppSecPhase::Running {
            return;
        }
        if let Some(disconnect) = self.disconnect.get() {
            disconnect(session_id, client_id);
        }
    }

    pub(crate) async fn shutdown(&self) {
        loop {
            match self.phase() {
                AppSecPhase::Uninitialized => {
                    if self.transition(AppSecPhase::Uninitialized, AppSecPhase::Stopped) {
                        return;
                    }
                }
                AppSecPhase::Starting => self.wait_for_initialization().await,
                AppSecPhase::Running => {
                    if !self.transition(AppSecPhase::Running, AppSecPhase::ShuttingDown) {
                        continue;
                    }

                    let Some(shutdown) = self.shutdown.take() else {
                        error!("Running AppSec backend has no shutdown owner");
                        self.set_phase(AppSecPhase::Stopped);
                        return;
                    };

                    info!("Shutting down appsec backend");
                    shutdown.await;
                    info!("Appsec backend shutdown");
                    self.set_phase(AppSecPhase::Stopped);
                    return;
                }
                AppSecPhase::ShuttingDown | AppSecPhase::Stopped => return,
            }
        }
    }

    fn phase(&self) -> AppSecPhase {
        self.phase.load()
    }

    fn transition(&self, from: AppSecPhase, to: AppSecPhase) -> bool {
        self.phase.compare_exchange(from, to).is_ok()
    }

    fn set_phase(&self, phase: AppSecPhase) {
        self.phase.store(phase);
    }

    fn finish_initialization(&self, phase: AppSecPhase) {
        self.set_phase(phase);
        self.initialization_finished.notify_waiters();
    }

    async fn wait_for_initialization(&self) {
        let notified = self.initialization_finished.notified();
        if self.phase() == AppSecPhase::Starting {
            notified.await;
        }
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

/// A slot that a value can be published into and taken out of, each by at
/// most one thread at a time. After a `take`, the slot is empty and may be
/// published to again.
pub struct TakeSlot<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum TakeSlotState {
    Empty,
    Writing,
    Ready,
    Taking,
}

// SAFETY: the slot transfers `T` by value between threads and never hands
// out shared references to it, so `T: Send` suffices for both.
unsafe impl<T: Send> Send for TakeSlot<T> {}
unsafe impl<T: Send> Sync for TakeSlot<T> {}

impl<T> TakeSlot<T> {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(TakeSlotState::Empty as u8),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Sets the value in the slot.
    ///
    /// Returns an error if the slot is already set.
    /// Regardless, after returning, the value is set (though not necessarily the
    /// one provided as argument), unless of course it is taken in the interim.
    pub fn set(&self, value: T) -> Result<(), T> {
        loop {
            // Failure: Acquire so that observing READY (and returning Err)
            // establishes a happens-before with the value's initialization,
            // matching `OnceLock::set`'s guarantee.
            match self.state.compare_exchange_weak(
                TakeSlotState::Empty as u8,
                TakeSlotState::Writing as u8,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // SAFETY: holding the WRITING state gives us exclusive
                    // access to the (uninitialized) slot.
                    unsafe { (*self.value.get()).write(value) };
                    // Release pairs with the Acquire CAS in `take`: the
                    // taker observes a fully initialized value.
                    self.state
                        .store(TakeSlotState::Ready as u8, Ordering::Release);
                    return Ok(());
                }
                Err(state) if state == TakeSlotState::Ready as u8 => return Err(value),
                Err(_) => {
                    // WRITING: another publisher is mid-initialization; wait
                    // for it to reach READY/EMPTY, then return Err. (Also absorbs
                    // spurious `compare_exchange_weak` failures.)
                    core::hint::spin_loop();
                }
            }
        }
    }

    pub fn take(&self) -> Option<T> {
        // Acquire pairs with the Release store of READY in `publish`.
        self.state
            .compare_exchange(
                TakeSlotState::Ready as u8,
                TakeSlotState::Taking as u8,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()?;
        // SAFETY: holding the TAKING state gives us exclusive access to the
        // slot, and READY guarantees it holds an initialized value.
        let value = unsafe { (*self.value.get()).assume_init_read() };
        self.state
            .store(TakeSlotState::Empty as u8, Ordering::Release);
        Some(value)
    }
}

impl<T> Drop for TakeSlot<T> {
    fn drop(&mut self) {
        // &mut self: no concurrent publisher/taker can exist, so the state
        // is either EMPTY or READY (the guard states are never observable
        // here — nothing panics between entering and leaving them).
        if *self.state.get_mut() == TakeSlotState::Ready as u8 {
            // SAFETY: READY means the slot holds an initialized value.
            unsafe { (*self.value.get()).assume_init_drop() };
        }
    }
}
impl<T> Default for TakeSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Factory result for one started helper.
pub struct AppSecBackend {
    send_message: AppSecSendMessage,
    disconnect: AppSecDisconnect,
    shutdown: AppSecShutdownFuture,
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
            shutdown,
        }
    }
}
