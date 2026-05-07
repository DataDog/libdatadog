// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sleep capability trait.
//!
//! Abstracts async sleep so that native code can use `tokio::time::sleep`
//! while wasm delegates to `setTimeout` via `JsFuture`.

use crate::maybe_send::MaybeSend;
use core::future::Future;
use std::time::Duration;

pub trait SleepCapability: Clone + std::fmt::Debug {
    /// Construct a new sleeper.
    ///
    /// Stateless impls return a unit struct; stateful impls (mock clocks,
    /// virtual time sources, etc.) should return a sensible default. Callers
    /// that don't have an instance handy can use the static-style
    /// `C::new().sleep(duration)` pattern, mirroring `HttpClientCapability`'s
    /// `new_client()` + `request(&self)` shape.
    fn new() -> Self;

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend;
}
