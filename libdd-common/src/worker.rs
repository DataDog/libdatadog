// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Trait representing a generic worker.
///
/// The worker runs an async looping function running periodic tasks.
///
/// This trait can be used to provide wrapper around a worker.
pub trait Worker {
    /// Main worker loop
    fn run(&mut self) -> impl std::future::Future<Output = ()> + Send;
}
