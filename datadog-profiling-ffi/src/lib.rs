// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(all(feature = "symbolizer", not(target_os = "windows")))]
pub use symbolizer_ffi::*;

mod arc_handle;
mod exporter;
mod profile_handle;
mod profiles;
mod status;
mod string_storage;

pub use arc_handle::*;
pub use profile_handle::*;
pub use status::*;

// re-export crashtracker ffi
#[cfg(feature = "crashtracker-ffi")]
pub use datadog_crashtracker_ffi::*;

// re-export telemetry ffi
#[cfg(feature = "ddtelemetry-ffi")]
pub use ddtelemetry_ffi::*;

#[cfg(feature = "data-pipeline-ffi")]
#[allow(unused_imports)]
pub use data_pipeline_ffi::*;

// re-export ddsketch ffi
#[cfg(feature = "ddsketch-ffi")]
#[allow(unused_imports)]
pub use ddsketch_ffi::*;

// re-export library-config ffi
#[cfg(feature = "datadog-library-config-ffi")]
pub use datadog_library_config_ffi::*;

// re-export log ffi
#[cfg(feature = "datadog-log-ffi")]
pub use datadog_log_ffi::*;

// re-export tracer metadata functions
#[cfg(feature = "ddcommon-ffi")]
pub use ddcommon_ffi::*;
