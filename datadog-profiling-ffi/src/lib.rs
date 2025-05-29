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
mod profiles;
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

// re-export tracer metadata functions
#[cfg(feature = "ddcommon-ffi")]
pub use ddcommon_ffi::*;

pub use manager::*;
