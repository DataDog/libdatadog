// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

mod client;
mod ffi_utils;
mod profiler_manager;
mod samples;
#[cfg(test)]
mod tests;

pub use client::ManagedProfilerClient;
pub use profiler_manager::{ManagedSampleCallbacks, ProfilerManager};
pub use samples::{SampleChannels, SendSample};
