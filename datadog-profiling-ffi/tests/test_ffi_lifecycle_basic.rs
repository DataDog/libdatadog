mod test_utils;
use datadog_profiling_ffi::{
    ddog_prof_Profile_new, ddog_prof_ProfilerClient_drop, ddog_prof_ProfilerManager_pause,
    ddog_prof_ProfilerManager_reset_for_testing, ddog_prof_ProfilerManager_restart_in_parent,
    ddog_prof_ProfilerManager_start, ddog_prof_ProfilerManager_terminate, ManagedSampleCallbacks,
    ProfileNewResult, ProfilerManagerConfig, Slice, ValueType, VoidResult,
};
use test_utils::*;

#[test]
fn test_ffi_lifecycle_basic() {
    println!("[test] Starting basic lifecycle test");
    // Reset global state for this test
    unsafe { ddog_prof_ProfilerManager_reset_for_testing() }.unwrap();
    // Reset upload count for this test
    UPLOAD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

    // Create a profile
    println!("[test] Creating profile");
    let sample_types = [ValueType::new("samples", "count")];
    let profile_result = unsafe { ddog_prof_Profile_new(Slice::from(&sample_types[..]), None) };
    println!("[test] Profile created");
    let mut profile = match profile_result {
        ProfileNewResult::Ok(p) => p,
        ProfileNewResult::Err(e) => {
            panic!("Failed to create profile: {e}")
        }
    };

    // Create sample callbacks
    println!("[test] Creating sample callbacks");
    let sample_callbacks = ManagedSampleCallbacks::new(test_converter, test_reset, test_drop);

    // Create config with very long intervals to avoid timer issues
    println!("[test] Creating config");
    let config = ProfilerManagerConfig {
        channel_depth: 10,
        cpu_sampling_interval_ms: 10000, // 10 seconds - very long
        upload_interval_ms: 10000,       // 10 seconds - very long
    };

    // Start the profiler manager using FFI
    println!("[test] Calling ddog_prof_ProfilerManager_start");
    let client_result = unsafe {
        ddog_prof_ProfilerManager_start(
            &mut profile,
            test_cpu_sampler_callback,
            test_upload_callback,
            sample_callbacks,
            config,
        )
    };
    println!("[test] ddog_prof_ProfilerManager_start returned");

    let mut client_handle = match client_result {
        ddcommon_ffi::Result::Ok(handle) => handle,
        ddcommon_ffi::Result::Err(e) => panic!("Failed to start profiler manager: {e}"),
    };

    println!("[test] Profiler manager started successfully");

    // Pause immediately without sending any samples
    println!("[test] Calling ddog_prof_ProfilerManager_pause");
    let pause_result = unsafe { ddog_prof_ProfilerManager_pause() };
    println!("[test] ddog_prof_ProfilerManager_pause returned");
    match pause_result {
        VoidResult::Ok => println!("[test] Profiler manager paused successfully"),
        VoidResult::Err(e) => panic!("Failed to pause profiler manager: {e}"),
    }

    // Restart the profiler manager in parent (preserves profile data)
    println!("[test] Calling ddog_prof_ProfilerManager_restart_in_parent");
    let restart_result = unsafe { ddog_prof_ProfilerManager_restart_in_parent() };
    println!("[test] ddog_prof_ProfilerManager_restart_in_parent returned");
    let mut new_client_handle = match restart_result {
        ddcommon_ffi::Result::Ok(handle) => {
            println!("[test] Profiler manager restarted successfully");
            handle
        }
        ddcommon_ffi::Result::Err(e) => panic!("Failed to restart profiler manager: {e}"),
    };

    // Terminate the profiler manager immediately
    println!("[test] Calling ddog_prof_ProfilerManager_terminate");
    let terminate_result = unsafe { ddog_prof_ProfilerManager_terminate() };
    println!("[test] ddog_prof_ProfilerManager_terminate returned");
    let _final_profile_handle = match terminate_result {
        ddcommon_ffi::Result::Ok(handle) => {
            println!("[test] Profiler manager terminated successfully");
            handle
        }
        ddcommon_ffi::Result::Err(e) => panic!("Failed to terminate profiler manager: {e}"),
    };

    // Drop the client handles
    println!("[test] Dropping first client handle");
    let drop_result = unsafe { ddog_prof_ProfilerClient_drop(&mut client_handle) };
    match drop_result {
        VoidResult::Ok => println!("[test] First client handle dropped successfully"),
        VoidResult::Err(e) => println!("Warning: failed to drop first client handle: {e}"),
    }

    println!("[test] Dropping second client handle");
    let drop_result2 = unsafe { ddog_prof_ProfilerClient_drop(&mut new_client_handle) };
    match drop_result2 {
        VoidResult::Ok => println!("[test] Second client handle dropped successfully"),
        VoidResult::Err(e) => println!("Warning: failed to drop second client handle: {e}"),
    }

    println!("[test] Basic lifecycle test completed successfully");
}
