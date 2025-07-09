// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
mod test_utils;
use datadog_profiling_ffi::{
    ddog_prof_Profile_new, ddog_prof_ProfilerClient_drop, ddog_prof_ProfilerManager_enqueue_sample,
    ddog_prof_ProfilerManager_pause, ddog_prof_ProfilerManager_reset_for_testing,
    ddog_prof_ProfilerManager_restart_in_child, ddog_prof_ProfilerManager_start,
    ddog_prof_ProfilerManager_terminate, ManagedSampleCallbacks, ProfileNewResult,
    ProfilerManagerConfig, Slice, ValueType, VoidResult,
};
use ddcommon_ffi::ToInner;
use std::ffi::c_void;
use test_utils::*;

#[test]
fn test_ffi_fork_child_only() {
    println!("[test] Starting fork child-only test");

    // Reset global state for this test
    unsafe { ddog_prof_ProfilerManager_reset_for_testing() }.unwrap();
    // Reset upload count for this test
    UPLOAD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

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

    // Create config with short intervals for testing
    let config = ProfilerManagerConfig {
        channel_depth: 10,
        cpu_sampling_interval_ms: 50, // 50ms for faster testing
        upload_interval_ms: 100,      // 100ms for faster testing
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

    // Send a sample before forking
    println!("[test] Sending sample before fork");
    let test_sample = create_test_sample(42);
    let sample_ptr = Box::into_raw(Box::new(test_sample)) as *mut c_void;

    let enqueue_result =
        unsafe { ddog_prof_ProfilerManager_enqueue_sample(&mut client_handle, sample_ptr) };
    match enqueue_result {
        VoidResult::Ok => println!("[test] Sample enqueued successfully before fork"),
        VoidResult::Err(e) => panic!("Failed to enqueue sample before fork: {e}"),
    }

    // Give the manager time to process
    std::thread::sleep(std::time::Duration::from_millis(50));

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
            // Child process - test restart_in_child
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

            // Send a sample in child process
            println!("[child] Sending sample in child process");
            let child_sample = create_test_sample(100);
            let child_sample_ptr = Box::into_raw(Box::new(child_sample)) as *mut c_void;

            let child_enqueue_result = unsafe {
                ddog_prof_ProfilerManager_enqueue_sample(&mut child_client_handle, child_sample_ptr)
            };
            match child_enqueue_result {
                VoidResult::Ok => println!("[child] Sample enqueued successfully in child"),
                VoidResult::Err(e) => panic!("[child] Failed to enqueue sample in child: {e}"),
            }

            // Give the manager time to process
            std::thread::sleep(std::time::Duration::from_millis(50));

            // Terminate the profiler manager in child
            println!("[child] Terminating profiler manager in child");
            let terminate_result = unsafe { ddog_prof_ProfilerManager_terminate() };
            let mut final_profile_handle = match terminate_result {
                ddcommon_ffi::Result::Ok(handle) => {
                    println!("[child] Profiler manager terminated successfully in child");
                    handle
                }
                ddcommon_ffi::Result::Err(e) => {
                    panic!("[child] Failed to terminate profiler manager in child: {e}")
                }
            };

            // Check that the expected sample is present in the final profile
            let profile_result = unsafe { final_profile_handle.take() };
            assert_profile_has_sample_values(profile_result, &[100]);

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
            // Parent process - just wait for child and clean up
            println!("[parent] Parent process continuing, child PID: {child_pid}");

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
                println!("[parent] Child process terminated by signal");
            }

            // Drop the original client handle
            let drop_result = unsafe { ddog_prof_ProfilerClient_drop(&mut client_handle) };
            match drop_result {
                VoidResult::Ok => println!("[parent] Original client handle dropped successfully"),
                VoidResult::Err(e) => {
                    println!("[parent] Warning: failed to drop original client handle: {e}")
                }
            }

            println!("[parent] Parent process completed successfully");
        }
    }
}
