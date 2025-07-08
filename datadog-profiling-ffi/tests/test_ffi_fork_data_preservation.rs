// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod test_utils;
use datadog_profiling_ffi::{
    ddog_prof_Profile_new, ddog_prof_ProfilerClient_drop, ddog_prof_ProfilerManager_enqueue_sample,
    ddog_prof_ProfilerManager_pause, ddog_prof_ProfilerManager_reset_for_testing,
    ddog_prof_ProfilerManager_restart_in_child, ddog_prof_ProfilerManager_restart_in_parent,
    ddog_prof_ProfilerManager_start, ddog_prof_ProfilerManager_terminate, ManagedSampleCallbacks,
    ProfileNewResult, ProfilerManagerConfig, Slice, ValueType, VoidResult,
};
use ddcommon_ffi::ToInner;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use test_utils::*;

// Global counter to track uploads in different processes
static PARENT_UPLOAD_COUNT: AtomicUsize = AtomicUsize::new(0);
static CHILD_UPLOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

pub extern "C" fn parent_upload_callback(
    _profile: *mut ddcommon_ffi::Handle<datadog_profiling::internal::Profile>,
    _token: &mut std::option::Option<tokio_util::sync::CancellationToken>,
) {
    let upload_count = PARENT_UPLOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    println!("[parent_upload_callback] called, count: {upload_count}");
}

pub extern "C" fn child_upload_callback(
    _profile: *mut ddcommon_ffi::Handle<datadog_profiling::internal::Profile>,
    _token: &mut std::option::Option<tokio_util::sync::CancellationToken>,
) {
    let upload_count = CHILD_UPLOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    println!("[child_upload_callback] called, count: {upload_count}");
}

#[test]
fn test_ffi_fork_data_preservation() {
    println!("[test] Starting fork data preservation test");

    // Reset global state for this test
    unsafe { ddog_prof_ProfilerManager_reset_for_testing() }.unwrap();
    // Reset upload counts for this test
    UPLOAD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    PARENT_UPLOAD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    CHILD_UPLOAD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

    // Create a profile
    println!("[test] Creating profile");
    let sample_types = [ValueType::new("samples", "count")];
    let profile_result = unsafe { ddog_prof_Profile_new(Slice::from(&sample_types[..]), None) };
    let mut profile = match profile_result {
        ProfileNewResult::Ok(p) => p,
        ProfileNewResult::Err(e) => {
            panic!("Failed to create profile: {e}")
        }
    };

    // Create sample callbacks
    let sample_callbacks = ManagedSampleCallbacks::new(test_converter, test_reset, test_drop);

    // Create config with very short intervals to trigger uploads quickly
    let config = ProfilerManagerConfig {
        channel_depth: 10,
        cpu_sampling_interval_ms: 10, // 10ms for very fast testing
        upload_interval_ms: 100_000,  // 100 seconds - prevent uploads
    };

    // Start the profiler manager
    println!("[test] Starting profiler manager");
    let client_result = unsafe {
        ddog_prof_ProfilerManager_start(
            &mut profile,
            test_cpu_sampler_callback,
            test_upload_callback,
            sample_callbacks,
            config,
        )
    };

    let mut client_handle = match client_result {
        ddcommon_ffi::Result::Ok(handle) => handle,
        ddcommon_ffi::Result::Err(e) => panic!("Failed to start profiler manager: {e}"),
    };

    // Send multiple samples before forking to accumulate data
    println!("[test] Sending samples before fork");
    for i in 0..5 {
        let test_sample = create_test_sample(42 + i);
        let sample_ptr = Box::into_raw(Box::new(test_sample)) as *mut c_void;

        let enqueue_result =
            unsafe { ddog_prof_ProfilerManager_enqueue_sample(&mut client_handle, sample_ptr) };
        match enqueue_result {
            VoidResult::Ok => println!("[test] Sample {i} enqueued successfully before fork"),
            VoidResult::Err(e) => panic!("Failed to enqueue sample {i} before fork: {e}"),
        }
    }

    // Give the manager time to process and potentially upload
    println!("[test] Waiting for processing before fork");
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Pause the profiler manager before forking
    println!("[test] Pausing profiler manager before fork");
    let pause_result = unsafe { ddog_prof_ProfilerManager_pause() };
    match pause_result {
        VoidResult::Ok => println!("[test] Profiler manager paused successfully"),
        VoidResult::Err(e) => panic!("Failed to pause profiler manager: {e}"),
    }

    // Fork the process
    println!("[test] Forking process");
    match unsafe { libc::fork() } {
        -1 => panic!("Failed to fork"),
        0 => {
            // Child process - should restart with fresh profile (discards previous data)
            println!("[child] Child process started");

            // Child should restart with fresh profile (discards previous data)
            println!("[child] Restarting profiler manager in child");
            let restart_result = unsafe { ddog_prof_ProfilerManager_restart_in_child() };
            let mut child_client_handle = match restart_result {
                ddcommon_ffi::Result::Ok(handle) => {
                    println!("[child] Profiler manager restarted successfully in child");
                    handle
                }
                ddcommon_ffi::Result::Err(e) => {
                    panic!("[child] Failed to restart profiler manager in child: {e}")
                }
            };

            // Send a few samples in child process
            println!("[child] Sending samples in child process");
            for i in 0..3 {
                let child_sample = create_test_sample(100 + i);
                let child_sample_ptr = Box::into_raw(Box::new(child_sample)) as *mut c_void;

                let child_enqueue_result = unsafe {
                    ddog_prof_ProfilerManager_enqueue_sample(
                        &mut child_client_handle,
                        child_sample_ptr,
                    )
                };
                match child_enqueue_result {
                    VoidResult::Ok => {
                        println!("[child] Sample {i} enqueued successfully in child")
                    }
                    VoidResult::Err(e) => {
                        panic!("[child] Failed to enqueue sample {i} in child: {e}")
                    }
                }
            }

            // Give the manager time to process and potentially upload
            println!("[child] Waiting for processing in child");
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Terminate the profiler manager in child (added back)
            println!("[child] Terminating profiler manager in child");
            let terminate_result = unsafe { ddog_prof_ProfilerManager_terminate() };
            let mut _final_profile_handle = match terminate_result {
                ddcommon_ffi::Result::Ok(handle) => {
                    println!("[child] Profiler manager terminated successfully in child");
                    handle
                }
                ddcommon_ffi::Result::Err(e) => {
                    panic!("[child] Failed to terminate profiler manager in child: {e}")
                }
            };
            // Extract the profile and assert expected values
            let profile_result = unsafe { _final_profile_handle.take() };
            assert_profile_has_sample_values(profile_result, &[100, 101, 102]);

            // Drop the child client handle
            let drop_result = unsafe { ddog_prof_ProfilerClient_drop(&mut child_client_handle) };
            match drop_result {
                VoidResult::Ok => println!("[child] Child client handle dropped successfully"),
                VoidResult::Err(e) => {
                    println!("[child] Warning: failed to drop child client handle: {e}")
                }
            }

            println!("[child] Child process completed successfully");
            std::process::exit(0);
        }
        child_pid => {
            // Parent process - should restart preserving profile data
            println!("[parent] Parent process continuing, child PID: {child_pid}");

            // Parent should restart preserving profile data
            println!("[parent] Restarting profiler manager in parent");
            let restart_result = unsafe { ddog_prof_ProfilerManager_restart_in_parent() };
            let mut parent_client_handle = match restart_result {
                ddcommon_ffi::Result::Ok(handle) => {
                    println!("[parent] Profiler manager restarted successfully in parent");
                    handle
                }
                ddcommon_ffi::Result::Err(e) => {
                    panic!("[parent] Failed to restart profiler manager in parent: {e}")
                }
            };

            // Send a few more samples in parent process
            println!("[parent] Sending samples in parent process");
            for i in 0..3 {
                let parent_sample = create_test_sample(200 + i);
                let parent_sample_ptr = Box::into_raw(Box::new(parent_sample)) as *mut c_void;

                let parent_enqueue_result = unsafe {
                    ddog_prof_ProfilerManager_enqueue_sample(
                        &mut parent_client_handle,
                        parent_sample_ptr,
                    )
                };
                match parent_enqueue_result {
                    VoidResult::Ok => {
                        println!("[parent] Sample {i} enqueued successfully in parent");
                        // Add debugging: try to print the profile contents after enqueue
                        // (This would require an FFI call to extract the profile, which we don't
                        // have, so just print a marker)
                        println!("[parent] (debug) Enqueued sample value {}", 200 + i);
                    }
                    VoidResult::Err(e) => {
                        panic!("[parent] Failed to enqueue sample {i} in parent: {e}")
                    }
                }
            }

            // Give the manager time to process and potentially upload
            println!("[parent] Waiting for processing in parent");
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Print a marker before terminate
            println!("[parent] (debug) About to terminate, expecting to see post-fork samples in profile");

            // Wait for child to complete
            println!("[parent] Waiting for child process to complete");
            let mut status = 0;
            let wait_result = unsafe { libc::waitpid(child_pid, &mut status, 0) };
            if wait_result == -1 {
                panic!("[parent] Failed to wait for child process");
            }

            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                println!("[parent] Child process exited with code: {exit_code}");
                assert_eq!(exit_code, 0, "Child process should exit successfully");
            } else {
                println!("[parent] Child process terminated by signal {status}");
            }

            println!(
                "[parent] Child process completed, parent profile state should still be intact"
            );

            // Terminate the profiler manager in parent
            println!("[parent] About to terminate profiler manager in parent");
            let terminate_result = unsafe { ddog_prof_ProfilerManager_terminate() };
            let mut final_profile_handle = match terminate_result {
                ddcommon_ffi::Result::Ok(handle) => {
                    println!("[parent] Profiler manager terminated successfully in parent");
                    handle
                }
                ddcommon_ffi::Result::Err(e) => {
                    panic!("[parent] Failed to terminate profiler manager in parent: {e}")
                }
            };

            // Check that the expected sample is present in the final profile
            let profile_result = unsafe { final_profile_handle.take() };
            let pprof = roundtrip_to_pprof(profile_result);
            println!("[debug] Profile contains {} samples", pprof.samples.len());
            for (i, sample) in pprof.samples.iter().enumerate() {
                println!("[debug] Sample {}: values = {:?}", i, sample.values);
            }

            // Check pre-fork values
            let mut found = [false; 5];
            for sample in &pprof.samples {
                for (i, &expected) in [42, 43, 44, 45, 46].iter().enumerate() {
                    if sample.values.contains(&expected) {
                        found[i] = true;
                    }
                }
            }
            for (i, &was_found) in found.iter().enumerate() {
                assert!(
                    was_found,
                    "Expected pre-fork sample value {} not found in profile",
                    [42, 43, 44, 45, 46][i]
                );
            }

            // Check for merged post-fork sample
            let mut found_merged = false;
            for sample in &pprof.samples {
                if sample.values.contains(&603) {
                    // Check function name
                    if let Some(loc_id) = sample.location_ids.first() {
                        let loc_obj = pprof.locations.iter().find(|l| l.id == *loc_id);
                        if let Some(loc_obj) = loc_obj {
                            let fn_id = loc_obj.lines.first().map(|l| l.function_id);
                            if let Some(fn_id) = fn_id {
                                let fn_obj = pprof.functions.iter().find(|f| f.id == fn_id);
                                if let Some(fn_obj) = fn_obj {
                                    // fn_obj.name is an index into the string table
                                    let name_idx = fn_obj.name as usize;
                                    if let Some(name) = pprof.string_table.get(name_idx) {
                                        if name == "unknown_function" {
                                            found_merged = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            assert!(found_merged, "Expected merged post-fork sample value 603 with function name 'unknown_function' not found in profile");

            // Drop the client handles
            let drop_result = unsafe { ddog_prof_ProfilerClient_drop(&mut client_handle) };
            match drop_result {
                VoidResult::Ok => println!("[parent] Original client handle dropped successfully"),
                VoidResult::Err(e) => {
                    println!("[parent] Warning: failed to drop original client handle: {e}")
                }
            }

            let drop_result2 = unsafe { ddog_prof_ProfilerClient_drop(&mut parent_client_handle) };
            match drop_result2 {
                VoidResult::Ok => println!("[parent] Parent client handle dropped successfully"),
                VoidResult::Err(e) => {
                    println!("[parent] Warning: failed to drop parent client handle: {e}")
                }
            }

            // Note: We don't require uploads in this test since we're using a long upload interval
            // to prevent premature uploads that could interfere with data preservation testing.
            // The test has already verified that samples are correctly preserved across fork
            // boundaries.
            let total_uploads = UPLOAD_COUNT.load(Ordering::SeqCst);
            println!("[parent] Total uploads across all processes: {total_uploads}");

            println!("[parent] Parent process completed successfully");
        }
    }
}
