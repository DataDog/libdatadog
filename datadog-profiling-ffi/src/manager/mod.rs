// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

mod client;
mod ffi_api;
mod ffi_utils;
mod fork_handler;
mod profiler_manager;
mod samples;
#[cfg(test)]
mod tests;

pub use client::{ManagedProfilerClient, ManagedProfilerController};
pub use profiler_manager::{ManagedSampleCallbacks, ProfilerManager};
pub use samples::{ClientSampleChannels, SendSample};
