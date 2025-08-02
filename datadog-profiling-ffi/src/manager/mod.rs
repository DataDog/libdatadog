// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

mod client;
pub mod ffi_api;
mod ffi_utils;
mod profiler_manager;
mod samples;
#[cfg(test)]
mod tests;

pub use client::ManagedProfilerClient;
pub use profiler_manager::{
    ManagedProfilerController, ManagedSampleCallbacks, ProfilerManager, ProfilerManagerConfig,
};
pub use samples::{ClientSampleChannels, SendSample};

// Re-export FFI functions for integration tests
pub use ffi_api::*;
