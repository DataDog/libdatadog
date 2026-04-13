// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native sleep implementation backed by `tokio::time::sleep`.

use core::future::Future;
use std::time::Duration;

use libdd_capabilities::maybe_send::MaybeSend;
use libdd_capabilities::sleep::SleepCapability;

#[derive(Clone, Debug)]
pub struct NativeSleepCapability;

impl SleepCapability for NativeSleepCapability {
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend {
        tokio::time::sleep(duration)
    }
}
