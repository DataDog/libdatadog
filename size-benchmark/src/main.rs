// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Force-link all FFI crates so their #[no_mangle] symbols are available to the
// extern "C" stubs in the generated calls.rs.
extern crate datadog_ffe_ffi;
extern crate datadog_profiling_ffi;
extern crate libdd_crashtracker_ffi;
extern crate libdd_data_pipeline_ffi;
extern crate libdd_shared_runtime_ffi;
extern crate libdd_telemetry_ffi;
extern crate symbolizer_ffi;

include!(concat!(env!("OUT_DIR"), "/calls.rs"));

fn main() {
    exercise_all();
    println!("done");
}
