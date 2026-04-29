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
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend;
}
