// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(all(feature = "symbolizer", not(target_os = "windows")))]
pub use symbolizer_ffi::*;

mod exporter;
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
