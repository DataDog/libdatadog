// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
pub mod agent_remote_config;
pub mod config;
pub mod interface;
pub mod one_way_shared_memory;
pub mod setup;
pub mod entry;
mod tracer;
mod self_telemetry;

pub use entry::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
