mod test_utils;
use datadog_profiling_ffi::{
    ddog_prof_Profile_new, ddog_prof_ProfilerClient_drop, ddog_prof_ProfilerManager_enqueue_sample,
    ddog_prof_ProfilerManager_reset_for_testing, ddog_prof_ProfilerManager_start,
    ddog_prof_ProfilerManager_terminate, ManagedSampleCallbacks, ProfileNewResult,
    ProfilerManagerConfig, Slice, ValueType, VoidResult,
};
use std::ffi::c_void;
use test_utils::*;

#[test]
fn test_ffi_start_terminate() {
    println!("[test] Starting simple start/terminate test");
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

    // Create config with very short intervals for testing
    println!("[test] Creating config");
    let config = ProfilerManagerConfig {
        channel_depth: 10,
        cpu_sampling_interval_ms: 50, // 50ms for faster testing
        upload_interval_ms: 100,      // 100ms for faster testing
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

    // Send a sample using FFI
    println!("[test] Sending sample");
    let test_sample = create_test_sample(42);
    let sample_ptr = Box::into_raw(Box::new(test_sample)) as *mut c_void;

    let enqueue_result =
        unsafe { ddog_prof_ProfilerManager_enqueue_sample(&mut client_handle, sample_ptr) };
    println!("[test] ddog_prof_ProfilerManager_enqueue_sample returned");

    match enqueue_result {
        VoidResult::Ok => println!("[test] Sample enqueued successfully"),
        VoidResult::Err(e) => panic!("Failed to enqueue sample: {e}"),
    }

    // Give the manager a very short time to process
    println!("[test] Sleeping briefly");
    std::thread::sleep(std::time::Duration::from_millis(50));
    println!("[test] Woke up");

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

    // Drop the client handle
    println!("[test] Dropping client handle");
    let drop_result = unsafe { ddog_prof_ProfilerClient_drop(&mut client_handle) };
    match drop_result {
        VoidResult::Ok => println!("[test] Client handle dropped successfully"),
        VoidResult::Err(e) => println!("Warning: failed to drop client handle: {e}"),
    }

    println!("[test] Simple start/terminate test completed successfully");
}
