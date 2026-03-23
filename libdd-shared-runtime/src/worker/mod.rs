// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod pausable_worker;
pub mod shared_runtime;

use async_trait::async_trait;

/// A background worker meant to be spawned on a [`SharedRuntime`](shared_runtime::SharedRuntime).
///
/// # Lifecycle
/// The worker's [`run`](Self::run) method is executed every time [`trigger`](Self::trigger)
/// returns. On startup [`initial_trigger`](Self::initial_trigger) is called before the first
/// [`run`](Self::run).
#[async_trait]
pub trait Worker: std::fmt::Debug {
    /// Main worker function
    ///
    /// Code in this function should always use timeout on long-running await calls to avoid
    /// blocking forks if an await call takes too long to complete.
    async fn run(&mut self);

    /// Function called between each `run` to wait for the next run.
    async fn trigger(&mut self);

    /// Alternative trigger called on start to provide custom behavior.
    /// Defaults to `trigger` behavior.
    async fn initial_trigger(&mut self) {
        self.trigger().await
    }

    /// Reset the worker state. Called in the child after a fork to cleanup parent state.
    fn reset(&mut self) {}

    /// Hook called after the worker has been paused (e.g. before a fork).
    /// Default is a no-op.
    async fn on_pause(&mut self) {}

    /// Hook called when the app is shutting down. Can be used to flush remaining data.
    async fn shutdown(&mut self) {}
}

// Blanket implementation for boxed trait objects
#[async_trait]
impl Worker for Box<dyn Worker + Send + Sync> {
    async fn run(&mut self) {
        (**self).run().await
    }

    async fn trigger(&mut self) {
        (**self).trigger().await
    }

    async fn initial_trigger(&mut self) {
        (**self).initial_trigger().await
    }

    fn reset(&mut self) {
        (**self).reset()
    }

    async fn on_pause(&mut self) {
        (**self).on_pause().await
    }

    async fn shutdown(&mut self) {
        (**self).shutdown().await
    }
}
