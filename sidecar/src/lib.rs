// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
pub mod agent_remote_config;
pub mod config;
pub mod entry;
pub mod interface;
pub mod one_way_shared_memory;
mod self_telemetry;
pub mod setup;
mod tracer;
#[cfg(feature = "tracing")]
pub mod log;
mod dump;

pub use entry::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
