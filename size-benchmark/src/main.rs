// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Pull in all FFI crates so their symbols are available to the extern "C" block below.
extern crate datadog_ffe_ffi;
extern crate datadog_profiling_ffi;
extern crate libdd_common_ffi;
extern crate libdd_crashtracker_ffi;
extern crate libdd_data_pipeline_ffi;
extern crate libdd_ddsketch_ffi;
extern crate libdd_library_config_ffi;
extern crate libdd_log_ffi;
extern crate libdd_shared_runtime_ffi;
extern crate libdd_telemetry_ffi;
extern crate symbolizer_ffi;

include!(concat!(env!("OUT_DIR"), "/fptrs.rs"));

fn main() {}
