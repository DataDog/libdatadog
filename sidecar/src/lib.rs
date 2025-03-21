// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod agent_remote_config;
pub mod config;
pub mod crashtracker;
mod dump;
pub mod entry;
#[cfg(feature = "tracing")]
pub mod log;
pub mod one_way_shared_memory;
mod self_telemetry;
pub mod setup;
pub mod shm_remote_config;
pub mod tracer;
mod watchdog;

pub use entry::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

pub mod service;
mod tokio_util;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use self::windows::*;

macro_rules! sidecar_version {
    () => {
        datadog_sidecar_macros::env_or_default!("SIDECAR_VERSION", env!("CARGO_PKG_VERSION"))
    };
}
pub(crate) use sidecar_version;
