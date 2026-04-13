// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Spawn capability trait.
//!
//! Abstracts task spawning so that native code can use `tokio::spawn`
//! while wasm delegates to `wasm_bindgen_futures::spawn_local` with a
//! `RemoteHandle` for join/cancel semantics.

use crate::maybe_send::MaybeSend;
use core::future::Future;

pub trait SpawnCapability: Clone + std::fmt::Debug {
    type JoinHandle<T: MaybeSend + 'static>: Future<Output = T> + MaybeSend;

    fn spawn<F, T>(&self, future: F) -> Self::JoinHandle<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static;
}
