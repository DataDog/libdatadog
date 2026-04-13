// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Spawn capability trait.
//!
//! Abstracts task spawning so that native code can use `tokio::spawn`
//! while wasm delegates to `wasm_bindgen_futures::spawn_local` with a
//! `RemoteHandle` for join/cancel semantics.

use crate::maybe_send::MaybeSend;
use core::fmt;
use core::future::Future;

/// Executor-agnostic error returned when a spawned task is aborted or panics.
#[derive(Debug)]
pub struct SpawnError {
    msg: String,
}

impl SpawnError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "spawned task failed: {}", self.msg)
    }
}

impl core::error::Error for SpawnError {}

pub trait SpawnCapability: Clone + std::fmt::Debug {
    /// Platform-specific context passed to [`spawn`](Self::spawn).
    ///
    /// On native this is typically `tokio::runtime::Handle` — the spawner uses
    /// it to schedule the future on the correct runtime. On wasm this is `()`
    /// because `spawn_local` does not need an external handle.
    type RuntimeContext;

    /// Handle to a spawned task.
    ///
    /// Awaiting the handle yields `Ok(T)` on success, or `Err(SpawnError)` if
    /// the task panicked or was aborted.
    type JoinHandle<T: MaybeSend + 'static>: Future<Output = Result<T, SpawnError>> + MaybeSend;

    fn spawn<F, T>(&self, future: F, ctx: &Self::RuntimeContext) -> Self::JoinHandle<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static;
}
