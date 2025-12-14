// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod exporter;
mod profile_error;
mod profile_status;
mod profiles;
mod string_storage;

pub use profile_error::*;
pub use profile_status::*;

#[cfg(all(feature = "symbolizer", not(target_os = "windows")))]
pub use symbolizer_ffi::*;

// re-export crashtracker ffi
#[cfg(feature = "crashtracker-ffi")]
pub use libdd_crashtracker_ffi::*;

// re-export telemetry ffi
#[cfg(feature = "ddtelemetry-ffi")]
pub use libdd_telemetry_ffi::*;

#[cfg(feature = "data-pipeline-ffi")]
#[allow(unused_imports)]
pub use libdd_data_pipeline_ffi::*;

// re-export ddsketch ffi
#[cfg(feature = "ddsketch-ffi")]
#[allow(unused_imports)]
pub use libdd_ddsketch_ffi::*;

// re-export library-config ffi
#[cfg(feature = "datadog-library-config-ffi")]
pub use libdd_library_config_ffi::*;

// re-export log ffi
#[cfg(feature = "datadog-log-ffi")]
pub use libdd_log_ffi::*;

// re-export ffe ffi
#[cfg(feature = "datadog-ffe-ffi")]
pub use datadog_ffe_ffi;

// re-export tracer metadata functions
#[cfg(feature = "ddcommon-ffi")]
pub use libdd_common_ffi::*;
