// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod agent_remote_config;
pub mod config;
mod dump;
pub mod entry;
#[cfg(feature = "tracing")]
pub mod log;
pub mod one_way_shared_memory;
mod self_telemetry;
pub mod setup;
mod tracer;
mod watchdog;

pub use entry::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

pub mod service;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use self::windows::*;
