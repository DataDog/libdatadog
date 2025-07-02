// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(all(feature = "symbolizer", not(target_os = "windows")))]
pub use symbolizer_ffi::*;

mod exporter;
mod manager;
pub mod profiles;
mod string_storage;

// re-export crashtracker ffi
#[cfg(feature = "crashtracker-ffi")]
pub use datadog_crashtracker_ffi::*;

// re-export telemetry ffi
#[cfg(feature = "ddtelemetry-ffi")]
pub use ddtelemetry_ffi::*;

#[cfg(feature = "data-pipeline-ffi")]
#[allow(unused_imports)]
pub use data_pipeline_ffi::*;

// re-export library-config ffi
#[cfg(feature = "datadog-library-config-ffi")]
pub use datadog_library_config_ffi::*;

// re-export log ffi
#[cfg(feature = "datadog-log-ffi")]
pub use datadog_log_ffi::*;

// re-export tracer metadata functions
#[cfg(feature = "ddcommon-ffi")]
pub use ddcommon_ffi::*;

pub use manager::*;

// Re-export for integration tests
pub use crate::manager::ffi_api::{
    ddog_prof_ProfilerClient_drop, ddog_prof_ProfilerManager_enqueue_sample,
    ddog_prof_ProfilerManager_pause, ddog_prof_ProfilerManager_reset_for_testing,
    ddog_prof_ProfilerManager_restart_in_parent, ddog_prof_ProfilerManager_start,
    ddog_prof_ProfilerManager_terminate,
};
pub use crate::manager::{ManagedSampleCallbacks, ProfilerManagerConfig, SendSample};
pub use crate::profiles::datatypes::{
    ddog_prof_Profile_new, Function, Location, ProfileNewResult, Sample, ValueType,
};
