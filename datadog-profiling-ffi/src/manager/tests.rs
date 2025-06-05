use std::{ffi::c_void, time::Duration};

use crate::profiles::datatypes::Sample;
use datadog_profiling::internal;
use tokio_util::sync::CancellationToken;

use super::{ManagedSampleCallbacks, ProfilerManager};
use crate::manager::profiler_manager::ProfilerManagerConfig;

extern "C" fn test_cpu_sampler_callback(_: *mut datadog_profiling::internal::Profile) {
    println!("cpu sampler callback");
}
extern "C" fn test_upload_callback(
    _: *mut datadog_profiling::internal::Profile,
    _: &mut Option<CancellationToken>,
) {
    println!("upload callback");
}
extern "C" fn test_sample_converter(_: *mut c_void) -> Sample<'static> {
    println!("sample converter");
    Sample {
        locations: ddcommon_ffi::Slice::empty(),
        values: ddcommon_ffi::Slice::empty(),
        labels: ddcommon_ffi::Slice::empty(),
    }
}
extern "C" fn test_reset_callback(_: *mut c_void) {
    println!("reset callback");
}
extern "C" fn test_drop_callback(_: *mut c_void) {
    println!("drop callback");
}

#[test]
fn test_the_thing() {
    let sample_types = [];
    let period = None;
    let profile = internal::Profile::new(&sample_types, period);
    let sample_callbacks = ManagedSampleCallbacks::new(
        test_sample_converter,
        test_reset_callback,
        test_drop_callback,
    );
    let config = ProfilerManagerConfig::default();
    let handle = ProfilerManager::start(
        profile,
        test_cpu_sampler_callback,
        test_upload_callback,
        sample_callbacks,
        config,
    )
    .expect("Failed to start profiler");
    println!("start");
    std::thread::sleep(Duration::from_secs(5));
    handle.shutdown().unwrap();
}
