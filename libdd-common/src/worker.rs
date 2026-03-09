// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;

/// Trait representing a generic worker.
///
/// # Lifecycle
/// The worker's `Self::run` method should be executed everytime the `Self::trigger` method returns.
/// On startup `Self::initial_trigger` should be called before `Self::run`.
#[async_trait]
pub trait Worker: std::fmt::Debug {
    /// Main worker function
    ///
    /// Code in this function should always use timeout on long-running await calls to avoid
    /// blocking forks if an await call takes too long to complete.
    async fn run(&mut self);

    /// Function called between each `run` to wait for the next run
    async fn trigger(&mut self);

    /// Alternative trigger called on start to provide custom behavior
    /// Defaults to `trigger` behavior.
    async fn initial_trigger(&mut self) {
        self.trigger().await
    }

    /// Reset the worker in the child after a fork
    fn reset(&mut self) {}

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

    async fn shutdown(&mut self) {
        (**self).shutdown().await
    }
}
