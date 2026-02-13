// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;

/// Trait representing a generic worker.
///
/// The worker runs an async looping function running periodic tasks.
///
/// This trait can be used to provide wrapper around a worker.
///
/// This trait is dyn-compatible thanks to the `async_trait` macro,
/// which allows it to be used as `Box<dyn Worker>`.
#[async_trait]
pub trait Worker {
    /// Main worker function
    async fn run(&mut self);

    /// Function called between each `run` to wait for the next run
    async fn trigger(&mut self);

    /// Alternative trigger called on start to provide custom behavior
    /// Can be used to trigger first run right away. Defaults to `trigger` behavior.
    async fn initial_trigger(&mut self) {
        self.trigger().await
    }

    /// Reset the worker in the child after a fork
    fn reset(&mut self) {
        return;
    }

    /// Hook called when the app is shutting down. Used to flush all data.
    fn shutdown(&mut self) {
        return;
    }
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

    fn shutdown(&mut self) {
        (**self).shutdown()
    }
}
