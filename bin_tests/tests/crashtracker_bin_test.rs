// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{
    build_artifacts,
    test_runner::{
        run_crash_no_op, run_crash_test_with_artifacts, CrashTestConfig, StandardArtifacts,
        ValidatorFn,
    },
    test_types::{CrashType, TestMode},
    validation::PayloadValidator,
    ArtifactType, ArtifactsBuild, BuildProfile,
};
use libdd_crashtracker::{
    CrashtrackerConfiguration, Metadata, SiCodes, SigInfo, SignalNames, StacktraceCollection,
};
use serde_json::Value;

/// Macro to generate simple crash tracking tests using the new infrastructure.
/// This replaces 16+ nearly identical test functions with a single declaration.
macro_rules! crash_tracking_tests {
    ($(($test_name:ident, $profile:expr, $mode:expr, $crash_type:expr)),* $(,)?) => {
        $(
            #[test]
            #[cfg_attr(miri, ignore)]
            fn $test_name() {
                run_standard_crash_test_refactored($profile, $mode, $crash_type);
            }
        )*
    };
}

// Generate all simple crash tracking tests using the macro
crash_tracking_tests! {
    (test_crash_tracking_bin_debug, BuildProfile::Debug, TestMode::DoNothing, CrashType::NullDeref),
    (test_crash_tracking_bin_sigpipe, BuildProfile::Debug, TestMode::SigPipe, CrashType::NullDeref),
    (test_crash_tracking_bin_sigchld, BuildProfile::Debug, TestMode::SigChld, CrashType::NullDeref),
    (test_crash_tracking_bin_sigchld_exec, BuildProfile::Debug, TestMode::SigChldExec, CrashType::NullDeref),
    (test_crash_tracking_bin_sigstack, BuildProfile::Release, TestMode::DoNothingSigStack, CrashType::NullDeref),
    (test_crash_tracking_bin_sigpipe_sigstack, BuildProfile::Release, TestMode::SigPipeSigStack, CrashType::NullDeref),
    (test_crash_tracking_bin_sigchld_sigstack, BuildProfile::Release, TestMode::SigChldSigStack, CrashType::NullDeref),
    (test_crash_tracking_bin_chained, BuildProfile::Release, TestMode::Chained, CrashType::NullDeref),
    (test_crash_tracking_bin_fork, BuildProfile::Release, TestMode::Fork, CrashType::NullDeref),
    (test_crash_tracking_bin_kill_sigabrt, BuildProfile::Release, TestMode::DoNothing, CrashType::KillSigAbrt),
    (test_crash_tracking_bin_kill_sigill, BuildProfile::Release, TestMode::DoNothing, CrashType::KillSigIll),
    (test_crash_tracking_bin_kill_sigbus, BuildProfile::Release, TestMode::DoNothing, CrashType::KillSigBus),
    (test_crash_tracking_bin_kill_sigsegv, BuildProfile::Release, TestMode::DoNothing, CrashType::KillSigSegv),
    (test_crash_tracking_bin_raise_sigabrt, BuildProfile::Release, TestMode::DoNothing, CrashType::RaiseSigAbrt),
    (test_crash_tracking_bin_raise_sigill, BuildProfile::Release, TestMode::DoNothing, CrashType::RaiseSigIll),
    (test_crash_tracking_bin_raise_sigbus, BuildProfile::Release, TestMode::DoNothing, CrashType::RaiseSigBus),
    (test_crash_tracking_bin_raise_sigsegv, BuildProfile::Release, TestMode::DoNothing, CrashType::RaiseSigSegv),
    (test_crash_tracking_bin_prechain_sigabrt, BuildProfile::Release, TestMode::PrechainAbort, CrashType::NullDeref),
}

/// Standard crash test runner using the new refactored infrastructure.
/// This eliminates the need for the old `test_crash_tracking_bin` function.
fn run_standard_crash_test_refactored(
    profile: BuildProfile,
    mode: TestMode,
    crash_type: CrashType,
) {
    let config = CrashTestConfig::new(profile, mode, crash_type);
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let crash_type_str = crash_type.as_str();
    let validator: ValidatorFn = Box::new(move |payload, fixtures| {
        // Standard validations using the fluent API
        PayloadValidator::new(payload).validate_counters()?;

        // Validate siginfo and error message
        let sig_info = &payload["sig_info"];
        assert_siginfo_message(sig_info, crash_type_str);

        let error = &payload["error"];
        assert_error_message(&error["message"], sig_info);

        // Validate telemetry
        validate_telemetry(&fixtures.crash_telemetry_path, crash_type_str)?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

// These tests below use the new infrastructure but require custom validation logic
// that doesn't fit the simple macro-generated pattern.

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_frame() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::RuntimeCallbackFrame,
        CrashType::NullDeref,
    );
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|payload, fixtures| {
        PayloadValidator::new(payload).validate_counters()?;

        let sig_info = &payload["sig_info"];
        assert_siginfo_message(sig_info, "null_deref");

        let error = &payload["error"];
        assert_error_message(&error["message"], sig_info);

        validate_runtime_callback_frame_data(payload);
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_thread_name() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::DoNothing,
        CrashType::NullDeref,
    );
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|payload, _fixtures| {
        let error = &payload["error"];
        let thread_name = error["thread_name"]
            .as_str()
            .expect("thread_name should be present");
        assert!(
            !thread_name.trim().is_empty(),
            "thread_name should not be empty: {thread_name:?}"
        );
        assert!(
            // Cutting `crashtracker_bin_test` to `crashtracker_bin` because linux
            // thread name is limited to 15 characters
            thread_name.contains("crashtracker_bi"),
            "thread_name should contain binary name: {thread_name:?}"
        );

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_string() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::RuntimeCallbackString,
        CrashType::NullDeref,
    );
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|payload, fixtures| {
        PayloadValidator::new(payload).validate_counters()?;

        let sig_info = &payload["sig_info"];
        assert_siginfo_message(sig_info, "null_deref");

        let error = &payload["error"];
        assert_error_message(&error["message"], sig_info);

        validate_runtime_callback_string_data(payload);
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_no_runtime_callback() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::DoNothing,
        CrashType::NullDeref,
    );
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|payload, fixtures| {
        PayloadValidator::new(payload).validate_counters()?;

        let sig_info = &payload["sig_info"];
        assert_siginfo_message(sig_info, "null_deref");

        let error = &payload["error"];
        assert_error_message(&error["message"], sig_info);

        validate_no_runtime_callback_data(payload);
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn test_collector_no_allocations_stacktrace_modes() {
    // (env_value, should_expect_log)
    let cases = [
        ("disabled", false),
        ("without_symbols", false),
        ("receiver_symbols", false),
        ("inprocess_symbols", true),
    ];

    for (env_value, expect_log) in cases {
        let detector_log_path = PathBuf::from("/tmp/preload_detector.log");

        // Clean up
        let _ = fs::remove_file(&detector_log_path);

        let config = CrashTestConfig::new(
            BuildProfile::Debug,
            TestMode::RuntimePreloadLogger,
            CrashType::NullDeref,
        )
        .with_env("DD_TEST_STACKTRACE_COLLECTION", env_value);

        let result = run_crash_no_op(&config);

        let log_exists = detector_log_path.exists();

        if expect_log {
            assert!(
                log_exists,
                "Expected allocation detection log for mode {env_value}"
            );
            if log_exists {
                if let Ok(bytes) = fs::read(&detector_log_path) {
                    eprintln!("{}", String::from_utf8_lossy(&bytes));
                }
            }
        } else {
            result.unwrap();
            assert!(
                !log_exists,
                "Did not expect allocation detection log for mode {env_value}"
            );
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_frame_invalid_utf8() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::RuntimeCallbackFrameInvalidUtf8,
        CrashType::NullDeref,
    );
    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|payload, fixtures| {
        PayloadValidator::new(payload).validate_counters()?;

        let sig_info = &payload["sig_info"];
        assert_siginfo_message(sig_info, "null_deref");

        let error = &payload["error"];
        assert_error_message(&error["message"], sig_info);

        validate_runtime_callback_frame_invalid_utf8_data(payload);
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_ping_timing_and_content() {
    // This test is identical to the simple donothing test
    run_standard_crash_test_refactored(
        BuildProfile::Release,
        TestMode::DoNothing,
        CrashType::NullDeref,
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_errors_intake_upload() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::DoNothing,
        CrashType::NullDeref,
    )
    .with_env("DD_CRASHTRACKING_ERRORS_INTAKE_ENABLED", "true");

    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|_payload, fixtures| {
        let errors_intake_path = fixtures.crash_profile_path.with_extension("errors");
        assert!(
            errors_intake_path.exists(),
            "Errors intake file should be created at {}",
            errors_intake_path.display()
        );

        let errors_intake_content =
            fs::read(&errors_intake_path).context("reading errors intake payload")?;

        assert_errors_intake_payload(&errors_intake_content, "null_deref");
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_errors_intake_crash_ping() {
    let config = CrashTestConfig::new(
        BuildProfile::Release,
        TestMode::DoNothing,
        CrashType::NullDeref,
    )
    .with_env("DD_CRASHTRACKING_ERRORS_INTAKE_ENABLED", "true");

    let artifacts = StandardArtifacts::new(config.profile);
    let artifacts_map = build_artifacts(&artifacts.as_slice()).unwrap();

    let validator: ValidatorFn = Box::new(|_payload, fixtures| {
        let errors_intake_path = fixtures.crash_profile_path.with_extension("errors");
        assert!(errors_intake_path.exists());

        let errors_intake_content =
            fs::read(&errors_intake_path).context("reading errors intake payload")?;

        assert_errors_intake_payload(&errors_intake_content, "null_deref");
        validate_telemetry(&fixtures.crash_telemetry_path, "null_deref")?;

        Ok(())
    });

    run_crash_test_with_artifacts(&config, &artifacts_map, &artifacts, validator).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
fn test_crash_tracking_errors_intake_uds_socket() {
    // This test requires special UDS socket setup, keeping the old implementation
    test_crash_tracking_bin_with_errors_intake_uds(
        BuildProfile::Release,
        "donothing",
        "null_deref",
    );
}

/// For some reason, the next two tests fail on MacOS, because the callstack cannot be collected.
/// We get this error:
/// thread 'test_crash_tracking_bin_segfault' (88268) panicked at
/// bin_tests/tests/crashtracker_bin_test.rs:250:5: got Ok("Unable to process line:
/// DD_CRASHTRACK_END_STACKTRACE. Error: Can't set non-existant stack complete\n")
#[test]
#[cfg_attr(miri, ignore)]
#[cfg(not(target_os = "macos"))]
fn test_crash_tracking_bin_panic() {
    test_crash_tracking_app("panic");
}

#[test]
#[cfg(not(target_os = "macos"))]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_segfault() {
    test_crash_tracking_app("segfault");
}

#[cfg(not(target_os = "macos"))]
fn test_crash_tracking_app(crash_type: &str) {
    use bin_tests::test_runner::run_custom_crash_test;

    // Set up custom artifacts: receiver + crashing_test_app with panic_abort
    let crashtracker_receiver = create_crashtracker_receiver(BuildProfile::Release);
    let crashing_app = create_crashing_app(BuildProfile::Debug, true);

    let artifacts_map = build_artifacts(&[&crashtracker_receiver, &crashing_app]).unwrap();

    // Create validator based on crash type
    let crash_type_owned = crash_type.to_owned();
    let validator: ValidatorFn = Box::new(move |payload, _fixtures| {
        let sig_info = &payload["sig_info"];
        let error = &payload["error"];

        match crash_type_owned.as_str() {
            "panic" => {
                let message = error["message"].as_str().unwrap();
                assert!(
                    message.contains("Process panicked with message") && message.contains("program panicked"),
                    "Expected panic message to contain 'Process panicked with message' and 'program panicked', got: {}",
                    message
                );
            }
            "segfault" => {
                assert_error_message(&error["message"], sig_info);
            }
            _ => unreachable!("Invalid crash type: {}", crash_type_owned),
        }

        Ok(())
    });

    run_custom_crash_test(
        &artifacts_map[&crashing_app],
        |cmd, fixtures| {
            cmd.arg(format!("file://{}", fixtures.crash_profile_path.display()))
                .arg(&artifacts_map[&crashtracker_receiver])
                .arg(&fixtures.output_dir)
                .arg(crash_type);
        },
        validator,
    )
    .unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(not(target_os = "macos"))] // Same restriction as other panic tests
fn test_crash_tracking_bin_panic_hook_after_fork() {
    test_panic_hook_mode(
        "panic_hook_after_fork",
        "message",
        Some("child panicked after fork"),
    );
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(not(target_os = "macos"))] // Same restriction as other panic tests
fn test_crash_tracking_bin_panic_hook_string() {
    test_panic_hook_mode("panic_hook_string", "message", Some("Panic with value: 42"));
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(not(target_os = "macos"))] // Same restriction as other panic tests
fn test_crash_tracking_bin_panic_hook_unknown_type() {
    test_panic_hook_mode(
        "panic_hook_unknown_type",
        "unknown type",
        None, // no panic message for unknown type
    );
}

/// Helper function to run panic hook tests with different payload types.
/// Note: Since tests are built with Debug profile, location is always expected.
#[cfg(not(target_os = "macos"))]
fn test_panic_hook_mode(mode: &str, expected_category: &str, expected_panic_message: Option<&str>) {
    use bin_tests::test_runner::run_custom_crash_test;

    // Set up custom artifacts: receiver + crashtracker_bin_test
    let crashtracker_receiver = create_crashtracker_receiver(BuildProfile::Release);
    let crashtracker_bin_test = create_crashtracker_bin_test(BuildProfile::Debug, true);

    let artifacts_map = build_artifacts(&[&crashtracker_receiver, &crashtracker_bin_test]).unwrap();

    let expected_category = expected_category.to_owned();
    let expected_panic_message = expected_panic_message.map(|s| s.to_owned());
    let validator: ValidatorFn = Box::new(move |payload, _fixtures| {
        // Verify the panic message is captured
        let error = &payload["error"];
        let message = error["message"].as_str().unwrap();

        // Check the message starts with "Process panicked with <category>"
        let expected_prefix = format!("Process panicked with {}", expected_category);
        assert!(
            message.starts_with(&expected_prefix),
            "Expected panic message to start with '{}', got: {}",
            expected_prefix,
            message
        );

        // Check the panic message if expected (the message passed to panic! macro)
        if let Some(ref panic_msg) = expected_panic_message {
            assert!(
                message.contains(panic_msg),
                "Expected panic message to contain '{}', got: {}",
                panic_msg,
                message
            );
        }

        // Check for location format (file:line:column) - always present in Debug builds
        // Location should end with pattern like " (path/file.rs:123:45)"
        let location_regex = regex::Regex::new(r" \(.+?:\d+:\d+\)$").unwrap();
        assert!(
            location_regex.is_match(message),
            "Expected panic message to end with location ' (file:line:column)', got: {}",
            message
        );

        Ok(())
    });

    run_custom_crash_test(
        &artifacts_map[&crashtracker_bin_test],
        |cmd, fixtures| {
            cmd.arg(format!("file://{}", fixtures.crash_profile_path.display()))
                .arg(&artifacts_map[&crashtracker_receiver])
                .arg(&fixtures.output_dir)
                .arg(mode)
                .arg("donothing"); // crash method (not used in panic hook tests)
        },
        validator,
    )
    .unwrap();
}

// ====================================================================================
// CALLSTACK VALIDATION TESTS - MIGRATED TO CUSTOM TEST RUNNER
// ====================================================================================
// These tests use `run_custom_crash_test` with the crashing_test_app artifact.

// This test is disabled for now on x86_64 musl and macos
// It seems that on aarch64 musl, libc has CFI which allows
// unwinding passed the signal frame.
// Don't forget to update the ignore condition for this and also
// `test_crash_tracking_callstack` when this is revisited.
#[test]
#[cfg(not(target_os = "macos"))]
#[cfg_attr(miri, ignore)]
fn test_crasht_tracking_validate_callstack() {
    test_crash_tracking_callstack()
}

// This test is disabled for now on x86_64 musl and macos for the reason mentioned above.
#[cfg(not(target_os = "macos"))]
fn test_crash_tracking_callstack() {
    use bin_tests::test_runner::run_custom_crash_test;

    // Set up custom artifacts: receiver + crashing_test_app (in Debug mode)
    let crashtracker_receiver = create_crashtracker_receiver(BuildProfile::Release);
    // compile in debug so we avoid inlining and can check the callchain
    let crashing_app = create_crashing_app(BuildProfile::Debug, false);

    let artifacts_map = build_artifacts(&[&crashtracker_receiver, &crashing_app]).unwrap();

    // Note: in Release, we do not have the crate and module name prepended to the function name
    // Here we compile the crashing app in Debug.
    let expected_functions = [
        "crashing_test_app::unix::fn3",
        "crashing_test_app::unix::fn2",
        "crashing_test_app::unix::fn1",
        "crashing_test_app::unix::main",
        "crashing_test_app::main",
    ];

    run_custom_crash_test(
        &artifacts_map[&crashing_app],
        |cmd, fixtures| {
            cmd.arg(format!("file://{}", fixtures.crash_profile_path.display()))
                .arg(&artifacts_map[&crashtracker_receiver])
                .arg(&fixtures.output_dir)
                .arg("segfault");
        },
        |payload, _fixtures| {
            // Use the new callstack validator
            PayloadValidator::new(payload).validate_callstack_functions(&expected_functions)?;
            Ok(())
        },
    )
    .unwrap();
}

fn validate_runtime_callback_frame_data(crash_payload: &Value) {
    // Look for runtime stack frames in the experimental section
    let experimental = crash_payload.get("experimental");
    assert!(
        experimental.is_some(),
        "Experimental section should be present in crash payload for runtime callback test"
    );

    let runtime_stack = experimental.unwrap().get("runtime_stack");
    assert!(
        runtime_stack.is_some(),
        "Runtime stack should be present in experimental section for frame mode"
    );

    let runtime_stack = runtime_stack.unwrap();

    // Check the format field
    assert_eq!(
        runtime_stack["format"].as_str().unwrap(),
        "Datadog Runtime Callback 1.0",
        "Runtime stack format should be correct"
    );

    // The runtime stack should have a frames array
    let frames = runtime_stack.get("frames");
    assert!(frames.is_some(), "Runtime stack should have frames array");

    let frames = frames.unwrap().as_array();
    assert!(frames.is_some(), "Runtime stack frames should be an array");

    let frames = frames.unwrap();
    assert!(
        frames.len() == 3,
        "Should have 3 runtime frames, got {}",
        frames.len()
    );

    // Validate the expected test frames
    let expected_functions = ["runtime_function_1", "runtime_function_2", "runtime_main"];
    let expected_files = ["script.py", "module.py", "main.py"];
    let expected_lines = [42, 100, 10];
    let expected_columns = [15, 8, 1];

    for (i, frame) in frames.iter().enumerate() {
        if let Some(function) = frame.get("function") {
            assert_eq!(
                function.as_str().unwrap(),
                expected_functions[i],
                "Frame {} function mismatch",
                i
            );
        }

        if let Some(file) = frame.get("file") {
            assert_eq!(
                file.as_str().unwrap(),
                expected_files[i],
                "Frame {} file mismatch",
                i
            );
        }

        if let Some(line) = frame.get("line") {
            assert_eq!(
                line.as_u64().unwrap() as u32,
                expected_lines[i],
                "Frame {} line mismatch",
                i
            );
        }

        if let Some(column) = frame.get("column") {
            assert_eq!(
                column.as_u64().unwrap() as u32,
                expected_columns[i],
                "Frame {} column mismatch",
                i
            );
        }
    }

    // Ensure stacktrace_string is null for frame mode
    assert!(
        runtime_stack.get("stacktrace_string").is_none()
            || runtime_stack["stacktrace_string"].is_null(),
        "Stacktrace string should be null/absent for frame mode"
    );
}

fn validate_runtime_callback_frame_invalid_utf8_data(crash_payload: &Value) {
    // Look for runtime stack frames in the experimental section
    let experimental = crash_payload.get("experimental");
    assert!(
        experimental.is_some(),
        "Experimental section should be present in crash payload for runtime callback test"
    );

    let runtime_stack = experimental.unwrap().get("runtime_stack");
    assert!(
        runtime_stack.is_some(),
        "Runtime stack should be present in experimental section for frame mode"
    );

    let runtime_stack = runtime_stack.unwrap();

    // Check the format field
    assert_eq!(
        runtime_stack["format"].as_str().unwrap(),
        "Datadog Runtime Callback 1.0",
        "Runtime stack format should be correct"
    );

    // The runtime stack should have a frames array
    let frames = runtime_stack.get("frames");
    assert!(frames.is_some(), "Runtime stack should have frames array");

    let frames = frames.unwrap().as_array();
    assert!(frames.is_some(), "Runtime stack frames should be an array");

    let frames = frames.unwrap();
    assert!(
        frames.len() == 7,
        "Should have 7 runtime frames (including invalid UTF-8 and null byte frames), got {}",
        frames.len()
    );

    // Validate frames with invalid UTF-8 are properly converted
    // Frame 1: Invalid UTF-8 in function name - should be converted with lossy conversion
    let frame1 = &frames[0];
    if let Some(function) = frame1.get("function") {
        let function_str = function.as_str().unwrap();
        assert!(
            function_str.contains("runtime_function_") && function_str.contains("_invalid"),
            "Frame 0 function should contain lossy-converted invalid UTF-8, got: {}",
            function_str
        );
        // Should contain replacement character (�) for invalid UTF-8
        assert!(
            function_str.contains("�"),
            "Frame 0 function should contain replacement character for invalid UTF-8, got: {}",
            function_str
        );
    }

    // Frame 2: Invalid UTF-8 in type name
    let frame2 = &frames[1];
    if let Some(type_name) = frame2.get("type_name") {
        let type_name_str = type_name.as_str().unwrap();
        assert!(
            type_name_str.contains("TestModule") && type_name_str.contains("TestClass"),
            "Frame 1 type_name should contain lossy-converted invalid UTF-8, got: {}",
            type_name_str
        );
        assert!(
            type_name_str.contains("�"),
            "Frame 1 type_name should contain replacement character for invalid UTF-8, got: {}",
            type_name_str
        );
    }

    // Frame 3: Invalid UTF-8 in file name
    let frame3 = &frames[2];
    if let Some(file) = frame3.get("file") {
        let file_str = file.as_str().unwrap();
        assert!(
            file_str.contains("script_") && file_str.contains(".py"),
            "Frame 2 file should contain lossy-converted invalid UTF-8, got: {}",
            file_str
        );
        assert!(
            file_str.contains("�"),
            "Frame 2 file should contain replacement character for invalid UTF-8, got: {}",
            file_str
        );
    }

    // Frame 4: Valid UTF-8 for comparison (should be unchanged)
    let frame4 = &frames[3];
    if let Some(function) = frame4.get("function") {
        assert_eq!(
            function.as_str().unwrap(),
            "valid_runtime_function",
            "Frame 3 function should be valid UTF-8"
        );
    }
    if let Some(file) = frame4.get("file") {
        assert_eq!(
            file.as_str().unwrap(),
            "valid_script.py",
            "Frame 3 file should be valid UTF-8"
        );
    }

    // Frame 5: Mixed invalid UTF-8 sequences
    let frame5 = &frames[4];
    if let Some(function) = frame5.get("function") {
        let function_str = function.as_str().unwrap();
        assert!(
            function_str.contains("func_") && function_str.contains("_invalid_overlong"),
            "Frame 4 function should contain lossy-converted invalid UTF-8, got: {}",
            function_str
        );
        assert!(
            function_str.contains("�"),
            "Frame 4 function should contain replacement character for invalid UTF-8, got: {}",
            function_str
        );
    }

    if let Some(type_name) = frame5.get("type_name") {
        let type_name_str = type_name.as_str().unwrap();
        assert!(
            type_name_str.contains("Class") && type_name_str.contains("Name"),
            "Frame 4 type_name should contain lossy-converted invalid UTF-8, got: {}",
            type_name_str
        );
        assert!(
            type_name_str.contains("�"),
            "Frame 4 type_name should contain replacement character for invalid UTF-8, got: {}",
            type_name_str
        );
    }

    // Frame 6: Null bytes (should be handled without truncation)
    let frame6 = &frames[5];
    if let Some(function) = frame6.get("function") {
        let function_str = function.as_str().unwrap();
        assert!(
            function_str.contains("func_with_") && function_str.contains("_null_byte"),
            "Frame 5 function should contain null byte content, got: {}",
            function_str
        );
        // Null bytes should be preserved (unlike C strings, Rust strings can contain them)
        assert!(
            function_str.contains('\0'),
            "Frame 5 function should preserve null byte, got: {}",
            function_str
        );
    }

    if let Some(type_name) = frame6.get("type_name") {
        let type_name_str = type_name.as_str().unwrap();
        assert!(
            type_name_str.contains("Type") && type_name_str.contains("WithNull"),
            "Frame 5 type_name should contain null byte content, got: {}",
            type_name_str
        );
        assert!(
            type_name_str.contains('\0'),
            "Frame 5 type_name should preserve null byte, got: {}",
            type_name_str
        );
    }

    // Frame 7: Empty fields (edge case) - should not have fields
    let frame7 = &frames[6];
    // Empty fields should be omitted from the JSON output
    assert!(
        frame7.get("function").is_none() || frame7["function"].as_str().unwrap_or("").is_empty(),
        "Frame 6 function should be empty or omitted"
    );
    assert!(
        frame7.get("type_name").is_none() || frame7["type_name"].as_str().unwrap_or("").is_empty(),
        "Frame 6 type_name should be empty or omitted"
    );
    assert!(
        frame7.get("file").is_none() || frame7["file"].as_str().unwrap_or("").is_empty(),
        "Frame 6 file should be empty or omitted"
    );

    // Ensure stacktrace_string is null for frame mode
    assert!(
        runtime_stack.get("stacktrace_string").is_none()
            || runtime_stack["stacktrace_string"].is_null(),
        "Stacktrace string should be null/absent for frame mode"
    );
}

fn validate_runtime_callback_string_data(crash_payload: &Value) {
    // Look for runtime stacktrace string in the experimental section
    let experimental = crash_payload.get("experimental");
    assert!(
        experimental.is_some(),
        "Experimental section should be present in crash payload for runtime callback test"
    );

    let runtime_stack = experimental.unwrap().get("runtime_stack");
    assert!(
        runtime_stack.is_some(),
        "{}",
        format!(
            "Runtime stack should be present in experimental section for string mode. Got: {:?}",
            experimental
        )
    );

    let runtime_stack = runtime_stack.unwrap();

    // Check the format field
    assert_eq!(
        runtime_stack["format"].as_str().unwrap(),
        "Datadog Runtime Callback 1.0",
        "Runtime stack format should be correct"
    );

    // The stacktrace_string should be present
    let stacktrace_string = runtime_stack.get("stacktrace_string");
    assert!(
        stacktrace_string.is_some(),
        "Runtime stack should have stacktrace_string field for string mode"
    );

    let stacktrace_str = stacktrace_string.unwrap().as_str();
    assert!(
        stacktrace_str.is_some(),
        "Runtime stacktrace_string should be a string"
    );

    let stacktrace_str = stacktrace_str.unwrap();
    // Validate that it contains the expected content from our test callback
    assert_eq!(
        stacktrace_str, "test_stacktrace_string",
        "Runtime stacktrace_string should be correct"
    );

    // Ensure frames array is empty for string mode
    let frames = runtime_stack.get("frames");
    if let Some(frames_array) = frames {
        if let Some(array) = frames_array.as_array() {
            assert!(
                array.is_empty(),
                "Frames array should be empty for string mode"
            );
        }
    }
}

fn validate_no_runtime_callback_data(crash_payload: &Value) {
    // Check if experimental section exists
    let experimental = crash_payload.get("experimental");

    if let Some(experimental) = experimental {
        // If experimental section exists, runtime_stack should not be present
        let runtime_stack = experimental.get("runtime_stack");
        assert!(
            runtime_stack.is_none(),
            "Runtime stack should NOT be present in experimental section when no callback is registered. Got: {:?}",
            runtime_stack
        );
    }
    // If experimental section doesn't exist at all, that's also fine -
    // it means no experimental features were added to the crash report
}

fn assert_error_message(message: &Value, sig_info: &Value) {
    let expected_message = format!(
        "Process terminated with {} ({})",
        sig_info["si_code_human_readable"].as_str().unwrap(),
        sig_info["si_signo_human_readable"].as_str().unwrap()
    );
    let message_str = message.as_str().unwrap_or("");
    assert_eq!(message_str, expected_message);
}

fn assert_siginfo_message(sig_info: &Value, crash_typ: &str) {
    match crash_typ {
        "null_deref" =>
        // On every platform other than OSX ARM, the si_code is 1: SEGV_MAPERR
        // On OSX ARM, its 2: SEGV_ACCERR
        {
            assert_eq!(sig_info["si_addr"], "0x0000000000000000");
            assert!(
                sig_info["si_code"] == 2 || sig_info["si_code"] == 1,
                "{sig_info:?}"
            );
            assert!(
                sig_info["si_code_human_readable"] == "SEGV_ACCERR"
                    || sig_info["si_code_human_readable"] == "SEGV_MAPERR",
                "{sig_info:?}"
            );
            assert_eq!(sig_info["si_signo"], libc::SIGSEGV);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGSEGV");
        }

        "kill_sigabrt" => {
            assert_eq!(sig_info["si_signo"], libc::SIGABRT);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGABRT");
            // https://vorner.github.io/2021/01/03/dark-side-of-posix-apis.html
            // OSX signal handling is the worst.
            #[cfg(not(target_os = "macos"))]
            assert_eq!(sig_info["si_code_human_readable"], "SI_USER");
        }
        "kill_sigsegv" => {
            assert_eq!(sig_info["si_signo"], libc::SIGSEGV);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGSEGV");
            // https://vorner.github.io/2021/01/03/dark-side-of-posix-apis.html
            // OSX signal handling is the worst.
            #[cfg(not(target_os = "macos"))]
            assert_eq!(sig_info["si_code_human_readable"], "SI_USER");
        }
        "kill_sigbus" => {
            assert_eq!(sig_info["si_signo"], libc::SIGBUS);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGBUS");
            // https://vorner.github.io/2021/01/03/dark-side-of-posix-apis.html
            // OSX signal handling is the worst.
            #[cfg(not(target_os = "macos"))]
            assert_eq!(sig_info["si_code_human_readable"], "SI_USER");
        }
        "kill_sigill" => {
            assert_eq!(sig_info["si_signo"], libc::SIGILL);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGILL");
            // https://vorner.github.io/2021/01/03/dark-side-of-posix-apis.html
            // OSX signal handling is the worst.
            #[cfg(not(target_os = "macos"))]
            assert_eq!(sig_info["si_code_human_readable"], "SI_USER");
        }
        "raise_sigabrt" => {
            assert_eq!(sig_info["si_signo"], libc::SIGABRT);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGABRT");
        }
        "raise_sigsegv" => {
            assert_eq!(sig_info["si_signo"], libc::SIGSEGV);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGSEGV");
        }
        "raise_sigbus" => {
            assert_eq!(sig_info["si_signo"], libc::SIGBUS);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGBUS");
        }
        "raise_sigill" => {
            assert_eq!(sig_info["si_signo"], libc::SIGILL);
            assert_eq!(sig_info["si_signo_human_readable"], "SIGILL");
        }
        _ => panic!("unexpected crash_typ {crash_typ}"),
    }
}

// Takes bytes of telemetry and deserializes it into a Value.
// The kind parameter determines which part of the telemetry to deserialize.
// - CrashReport: deserializes the first JSON payload (crash report)
// - Whole: deserializes the whole telemetry payload
// TODO (gyuheon): Refactor test helpers to have shared functionality for testing crash pings
/// Helper function to validate telemetry file (used by refactored tests)
fn validate_telemetry(telemetry_path: &Path, crash_type_str: &str) -> anyhow::Result<()> {
    let crash_telemetry = fs::read(telemetry_path).with_context(|| {
        format!(
            "reading crashtracker telemetry payload at {:?}",
            telemetry_path
        )
    })?;

    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_type_str);
        }
    }
    Ok(())
}

fn assert_telemetry_message(crash_telemetry: &[u8], crash_typ: &str) {
    let telemetry_payload: Value = serde_json::from_slice::<Value>(crash_telemetry)
        .context("deserializing whole telemetry payload to JSON")
        .unwrap();

    assert_eq!(telemetry_payload["request_type"], "logs");
    assert_eq!(
        serde_json::json!({
          "service_name": "foo",
          "service_version": "bar",
          "language_name": "native",
          "language_version": "unknown",
          "tracer_version": "unknown"
        }),
        telemetry_payload["application"]
    );
    assert_eq!(
        telemetry_payload["payload"]["logs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let log_entry = &telemetry_payload["payload"]["logs"][0];
    let tags_raw = log_entry["tags"].as_str().unwrap();
    let is_crash_ping = tags_raw.contains("is_crash_ping:true");

    let tags = tags_raw
        .split(',')
        .filter(|t| !t.starts_with("uuid:"))
        .map(|t| t.to_string())
        .collect::<std::collections::HashSet<_>>();

    let current_schema_version = libdd_crashtracker::CrashInfo::current_schema_version();

    let base_expected_tags: std::collections::HashSet<String> =
        std::collections::HashSet::from_iter([
            format!("data_schema_version:{current_schema_version}"),
            // "incomplete:false", // TODO: re-add after fixing musl unwinding
            "is_crash:true".to_string(),
            "profiler_collecting_sample:1".to_string(),
            "profiler_inactive:0".to_string(),
            "profiler_serializing:0".to_string(),
            "profiler_unwinding:0".to_string(),
        ]);

    match crash_typ {
        "null_deref" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_addr:0x0000000000000000"), "{tags:?}");
            assert!(
                tags.contains("si_code_human_readable:SEGV_ACCERR")
                    || tags.contains("si_code_human_readable:SEGV_MAPERR"),
                "{tags:?}"
            );
            assert!(tags.contains("si_signo_human_readable:SIGSEGV"), "{tags:?}");
            assert!(tags.contains("si_signo:11"), "{tags:?}");
            assert!(
                tags.contains("si_code:1") || tags.contains("si_code:2"),
                "{tags:?}"
            );
        }
        "kill_sigabrt" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGABRT"), "{tags:?}");
            assert!(tags.contains("si_signo:6"), "{tags:?}");
        }
        "kill_sigill" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGILL"), "{tags:?}");
            assert!(tags.contains("si_signo:4"), "{tags:?}");
        }
        "kill_sigbus" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGBUS"), "{tags:?}");
            // SIGBUS can be 7 or 10, depending on the os.
            assert!(
                tags.contains(format!("si_signo:{}", libc::SIGBUS).as_str()),
                "{tags:?}"
            );
        }
        "kill_sigsegv" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGSEGV"), "{tags:?}");
            assert!(tags.contains("si_signo:11"), "{tags:?}");
        }
        "raise_sigabrt" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGABRT"), "{tags:?}");
            assert!(tags.contains("si_signo:6"), "{tags:?}");
        }
        "raise_sigill" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGILL"), "{tags:?}");
            assert!(tags.contains("si_signo:4"), "{tags:?}");
        }
        "raise_sigbus" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGBUS"), "{tags:?}");
            // SIGBUS can be 7 or 10, depending on the os.
            assert!(
                tags.contains(format!("si_signo:{}", libc::SIGBUS).as_str()),
                "{tags:?}"
            );
        }
        "raise_sigsegv" => {
            assert!(base_expected_tags.is_subset(&tags), "{tags:?}");
            assert!(tags.contains("si_signo_human_readable:SIGSEGV"), "{tags:?}");
            assert!(tags.contains("si_signo:11"), "{tags:?}");
        }
        _ => panic!("{crash_typ}"),
    }

    assert_eq!(log_entry["is_sensitive"], true);

    if !is_crash_ping {
        let message_str = log_entry["message"]
            .as_str()
            .expect("Crash report telemetry should have a JSON message body");
        let crash_report_json: Value = serde_json::from_str(message_str)
            .expect("Crash report telemetry message should be valid JSON");

        assert_os_info_matches(
            &crash_report_json["os_info"],
            "telemetry crash report message",
        );
    }
}

fn assert_os_info_matches(os_info_val: &Value, context: &str) {
    assert!(
        os_info_val.is_object(),
        "os_info missing in {context}: {os_info_val:?}"
    );
    let expected_os_info = ::os_info::get();
    assert_eq!(
        os_info_val["architecture"].as_str().unwrap_or(""),
        expected_os_info.architecture().unwrap_or("unknown"),
        "mismatched architecture in os_info for {context}"
    );
    assert_eq!(
        os_info_val["bitness"].as_str().unwrap_or(""),
        expected_os_info.bitness().to_string(),
        "mismatched bitness in os_info for {context}"
    );
    assert_eq!(
        os_info_val["os_type"].as_str().unwrap_or(""),
        expected_os_info.os_type().to_string(),
        "mismatched os_type in os_info for {context}"
    );
    assert_eq!(
        os_info_val["version"].as_str().unwrap_or(""),
        expected_os_info.version().to_string(),
        "mismatched version in os_info for {context}"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
#[allow(clippy::zombie_processes)]
fn crash_tracking_empty_endpoint() {
    use std::os::unix::net::UnixListener;

    let (crashtracker_bin, crashtracker_receiver) = setup_crashtracking_crates(BuildProfile::Debug);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let socket_path = extend_path(fixtures.tmpdir.path(), "trace_agent.socket");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let mut child = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        // empty url, endpoint will be set to none
        .arg("")
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg("donothing")
        .arg("null_deref")
        .env("DD_CRASHTRACKING_ERRORS_INTAKE_ENABLED", "true")
        .env(
            "DD_TRACE_AGENT_URL",
            format!("unix://{}", socket_path.display()),
        )
        .spawn()
        .unwrap();

    // With parallel crash ping and crash report emission to both telemetry and errors intake, we
    // might receive requests in any order
    let (mut stream1, _) = listener.accept().unwrap();
    let body1 = read_http_request_body(&mut stream1);

    let _ = stream1.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream2, _) = listener.accept().unwrap();
    let body2 = read_http_request_body(&mut stream2);

    let _ = stream2.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream3, _) = listener.accept().unwrap();
    let body3 = read_http_request_body(&mut stream3);

    let _ = stream3.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream4, _) = listener.accept().unwrap();
    let body4 = read_http_request_body(&mut stream4);

    let _ = stream4.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");

    let all_bodies = [body1, body2, body3, body4];

    // Separate crash pings from crash reports
    let mut crash_pings = Vec::new();
    let mut crash_reports = Vec::new();

    for (i, body) in all_bodies.iter().enumerate() {
        if body.contains("is_crash_ping:true") {
            crash_pings.push((i + 1, body));
        } else if body.contains("is_crash:true") {
            crash_reports.push((i + 1, body));
        }
    }

    assert_eq!(
        crash_pings.len(),
        2,
        "Expected 2 crash pings (telemetry + errors intake), got {}",
        crash_pings.len()
    );
    assert_eq!(
        crash_reports.len(),
        2,
        "Expected 2 crash reports (telemetry + errors intake), got {}",
        crash_reports.len()
    );

    let telemetry_crash_ping = crash_pings
        .iter()
        .find(|(_, body)| body.contains("api_version") && body.contains("request_type"))
        .expect("Should have telemetry crash ping");
    assert_crash_ping_message(telemetry_crash_ping.1);

    let telemetry_crash_report = crash_reports
        .iter()
        .find(|(_, body)| body.contains("api_version") && body.contains("request_type"))
        .expect("Should have telemetry crash report");
    assert_telemetry_message(telemetry_crash_report.1.as_bytes(), "null_deref");

    let _ = child.wait();
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
fn test_receiver_emits_debug_logs_on_receiver_issue() -> anyhow::Result<()> {
    let receiver = ArtifactsBuild {
        name: "test_crashtracker_receiver".to_owned(),
        build_profile: BuildProfile::Debug,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        ..Default::default()
    };
    let artifacts = build_artifacts(&[&receiver])?;
    let fixtures = bin_tests::test_runner::TestFixtures::new()?;

    let missing_file = fixtures.output_dir.join("missing_additional_file.txt");

    let config = CrashtrackerConfiguration::new(
        vec![missing_file.display().to_string()],
        true,
        true,
        None,
        StacktraceCollection::WithoutSymbols,
        libdd_crashtracker::default_signals(),
        Some(std::time::Duration::from_millis(500)),
        None,
        true,
    )?;

    let metadata = Metadata {
        library_name: "libdatadog".to_owned(),
        library_version: "1.0.0".to_owned(),
        family: "native".to_owned(),
        tags: vec![
            "service:foo".into(),
            "service_version:bar".into(),
            "runtime-id:xyz".into(),
            "language:native".into(),
        ],
    };

    let siginfo = SigInfo {
        si_addr: None,
        si_code: 1,
        si_code_human_readable: SiCodes::SEGV_MAPERR,
        si_signo: libc::SIGSEGV,
        si_signo_human_readable: SignalNames::SIGSEGV,
    };

    let socket_path = fixtures.output_dir.join("trace_agent.socket");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)
        .context("binding unix socket for agent interception")?;
    listener
        .set_nonblocking(true)
        .context("setting socket nonblocking")?;

    let mut child = process::Command::new(&artifacts[&receiver])
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .env(
            "DD_TRACE_AGENT_URL",
            format!("unix://{}", socket_path.display()),
        )
        .spawn()
        .context("spawning receiver process")?;

    {
        let mut stdin = BufWriter::new(child.stdin.take().context("child stdin missing")?);
        for line in [
            "DD_CRASHTRACK_BEGIN_CONFIG".to_string(),
            serde_json::to_string(&config)?,
            "DD_CRASHTRACK_END_CONFIG".to_string(),
            "DD_CRASHTRACK_BEGIN_METADATA".to_string(),
            serde_json::to_string(&metadata)?,
            "DD_CRASHTRACK_END_METADATA".to_string(),
            "DD_CRASHTRACK_BEGIN_SIGINFO".to_string(),
            serde_json::to_string(&siginfo)?,
            "DD_CRASHTRACK_END_SIGINFO".to_string(),
            "UNEXPECTED_LINE_FROM_TEST".to_string(),
        ] {
            writeln!(stdin, "{line}")?;
        }
        stdin.flush()?;
    }

    let status = child.wait()?;
    assert!(
        status.success(),
        "receiver process should exit successfully"
    );

    let mut bodies = Vec::new();
    let mut found_receiver_issue_attach = false;
    let mut found_receiver_issue_unexpected = false;
    let mut found_receiver_issue_incomplete = false;
    let mut found_crash_report = false;

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);
    while start.elapsed() < timeout && bodies.len() < 16 {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let body = read_http_request_body(&mut stream);
                bodies.push(body.clone());
                // Update flags immediately to decide whether we can stop
                if body.contains("receiver_issue:attach_additional_file_error") {
                    found_receiver_issue_attach = true;
                }
                if body.contains("receiver_issue:unexpected_line") {
                    found_receiver_issue_unexpected = true;
                }
                if body.contains("receiver_issue:incomplete_stacktrace") {
                    found_receiver_issue_incomplete = true;
                }
                if body.contains("is_crash:true") {
                    found_crash_report = true;
                }
                if found_receiver_issue_attach
                    && found_receiver_issue_unexpected
                    && found_receiver_issue_incomplete
                    && found_crash_report
                {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.into()),
        }
    }

    let check_warn = |body: &str, tag: &str| {
        assert!(
            body.contains("is_crash_debug:true"),
            "expected crash debug tag for {tag} in body: {body}"
        );
        assert!(
            body.contains("\"level\":\"WARN\""),
            "expected WARN level for {tag} in body: {body}"
        );
    };

    for body in &bodies {
        if body.contains("receiver_issue:attach_additional_file_error") {
            check_warn(body, "attach_additional_file_error");
            found_receiver_issue_attach = true;
        }
        if body.contains("receiver_issue:unexpected_line") {
            check_warn(body, "unexpected_line");
            found_receiver_issue_unexpected = true;
        }
        if body.contains("receiver_issue:incomplete_stacktrace") {
            check_warn(body, "incomplete_stacktrace");
            found_receiver_issue_incomplete = true;
        }
        if body.contains("is_crash:true") {
            found_crash_report = true;
        }
    }

    assert!(
        found_receiver_issue_attach,
        "expected attach additional file debug telemetry log via agent socket; bodies: {:?}",
        bodies
    );
    assert!(
        found_receiver_issue_unexpected,
        "expected unexpected line debug telemetry log via agent socket; bodies: {:?}",
        bodies
    );
    assert!(
        found_receiver_issue_incomplete,
        "expected incomplete stacktrace debug telemetry log via agent socket; bodies: {:?}",
        bodies
    );
    assert!(
        found_crash_report,
        "expected crash report telemetry to be emitted alongside debug log; bodies: {:?}",
        bodies
    );

    Ok(())
}

fn read_http_request_body(stream: &mut impl Read) -> String {
    // The read call is not guaranteed to collect all available data.  On OSX it appears to grab
    // data in 8192 byte chunks.  This was not an issue when the size of a crashreport was below
    // there, but is a problem when the size is greater.
    // The obvious thing would be to use `read_to_end` or even `read_to_string`.
    // The problem with that is that we then block waiting for the client to close the channel,
    // which it doesn't do till it gets the response from us. Deadlock.  OOPS.
    // This is resolved by the timeout killing the receiver, but then we just fall back to the
    // 404 write failing.  See comment below.
    // This loop is a best effort attempt to fix the problem.
    // It can fail in two ways.
    // 1: There are exactly n*8192 bytes available.  We issue a read when there are 0 bytes
    //    available and deadlock.
    // 2: The read call decides not to return some but not all of the available bytes.  We exit
    //    early with a malformed string.
    // Since this is for testing, the risk of those are low, but if tests spuriously fails, that
    // is a good place to look.
    let mut out = vec![0; 65536];
    let blocksize = 8192;
    let mut left = 0;
    let mut right = blocksize;
    let mut total_read = 0;
    let mut done = false;
    while !(done) {
        let read = stream.read(&mut out[left..right]).unwrap();
        total_read += read;
        done = read != blocksize;
        left += blocksize;
        right += blocksize;
    }
    let resp = String::from_utf8_lossy(&out[..total_read]);
    let pos = resp.find("\r\n\r\n").unwrap();
    resp[pos + 4..].to_string()
}

fn assert_crash_ping_message(body: &str) {
    let telemetry_payload: serde_json::Value =
        serde_json::from_str(body).expect("Crash ping should be valid JSON");

    assert_eq!(telemetry_payload["request_type"], "logs");
    assert_eq!(
        telemetry_payload["payload"]["logs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let log_entry = &telemetry_payload["payload"]["logs"][0];

    let tags = log_entry["tags"].as_str().unwrap();
    assert!(
        tags.contains("is_crash_ping:true"),
        "Expected crash ping telemetry with is_crash_ping:true, but got tags: {tags}"
    );

    // Check for specific signal information in tags (for null_deref crash type)
    assert!(
        tags.contains("si_signo:11"),
        "Expected si_signo:11 (SIGSEGV) in tags, but got tags: {tags}"
    );
    assert!(
        tags.contains("si_signo_human_readable:SIGSEGV"),
        "Expected si_signo_human_readable:SIGSEGV in tags, but got tags: {tags}"
    );
    assert!(
        tags.contains("si_code_human_readable:SEGV_ACCERR")
            || tags.contains("si_code_human_readable:SEGV_MAPERR"),
        "Expected si_code_human_readable:SEGV_ACCERR or SEGV_MAPERR in tags, but got tags: {tags}"
    );

    let message_str = log_entry["message"]
        .as_str()
        .expect("Message field should exist as a string");
    let message_json: serde_json::Value =
        serde_json::from_str(message_str).expect("Message should be valid JSON");

    let crash_uuid = message_json["crash_uuid"]
        .as_str()
        .expect("crash_uuid should be present and be a string");
    assert!(!crash_uuid.is_empty(), "crash_uuid should be non-empty");

    assert_eq!(message_json["version"].as_str(), Some("1.0"));

    assert_eq!(message_json["kind"].as_str(), Some("Crash ping"));
}

// Old TestFixtures struct kept for UDS socket tests that weren't migrated
#[allow(dead_code)]
struct TestFixtures<'a> {
    tmpdir: tempfile::TempDir,
    crash_profile_path: PathBuf,
    crash_telemetry_path: PathBuf,
    output_dir: PathBuf,

    artifacts: HashMap<&'a ArtifactsBuild, PathBuf>,
}

fn setup_test_fixtures<'a>(crates: &[&'a ArtifactsBuild]) -> TestFixtures<'a> {
    let artifacts = build_artifacts(crates).unwrap();

    let tmpdir = tempfile::TempDir::new().unwrap();
    let dirpath = tmpdir.path();
    TestFixtures {
        crash_profile_path: extend_path(dirpath, "crash"),
        crash_telemetry_path: extend_path(dirpath, "crash.telemetry"),
        output_dir: dirpath.to_path_buf(),

        artifacts,
        tmpdir,
    }
}

fn setup_crashtracking_crates(
    crash_tracking_receiver_profile: BuildProfile,
) -> (ArtifactsBuild, ArtifactsBuild) {
    let crashtracker_bin = create_crashtracker_bin_test(crash_tracking_receiver_profile, false);
    let crashtracker_receiver = create_crashtracker_receiver(crash_tracking_receiver_profile);
    (crashtracker_bin, crashtracker_receiver)
}

// Helper functions for creating common artifact configurations

fn create_crashtracker_receiver(profile: BuildProfile) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "test_crashtracker_receiver".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        ..Default::default()
    }
}

#[cfg(not(target_os = "macos"))]
fn create_crashing_app(profile: BuildProfile, panic_abort: bool) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "crashing_test_app".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        panic_abort: if panic_abort { Some(true) } else { None },
        ..Default::default()
    }
}

fn create_crashtracker_bin_test(profile: BuildProfile, panic_abort: bool) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        panic_abort: if panic_abort { Some(true) } else { None },
        ..Default::default()
    }
}

#[cfg(unix)]
fn test_crash_tracking_bin_with_errors_intake_uds(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    // Try to create the standard agent UDS socket for testing
    let socket_path = std::path::Path::new("/var/run/datadog/apm.socket");

    // Create directory if it doesn't exist
    if let Some(parent) = socket_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            // Skip test if we can't create the directory
            eprintln!("Skipping UDS test - cannot create /var/run/datadog directory");
            return;
        }
    }

    // Remove socket if it exists from a previous run
    let _ = std::fs::remove_file(socket_path);

    // Create the Unix socket at the standard agent location
    let listener = match std::os::unix::net::UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("Skipping UDS test - cannot create socket at /var/run/datadog/apm.socket");
            return;
        }
    };

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg("") // Empty endpoint so both use agent detection
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .env("DD_CRASHTRACKING_ERRORS_INTAKE_ENABLED", "true")
        // Don't set DD_TRACE_AGENT_URL - let it auto-detect the UDS socket
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    match crash_typ {
        "kill_sigabrt" | "kill_sigill" | "null_deref" | "raise_sigabrt" | "raise_sigill" => {
            assert!(!exit_status.success())
        }
        "kill_sigbus" | "kill_sigsegv" | "raise_sigbus" | "raise_sigsegv" => {
            assert!(exit_status.success())
        }
        _ => unreachable!("{crash_typ} shouldn't happen"),
    }

    // Handle HTTP requests on the Unix socket - expect 4 requests total
    // 2 from telemetry (crash ping + crash report) and 2 from errors intake (crash ping + crash
    // report)
    let (mut stream1, _) = listener.accept().unwrap();
    let body1 = read_http_request_body(&mut stream1);
    let _ = stream1.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream2, _) = listener.accept().unwrap();
    let body2 = read_http_request_body(&mut stream2);
    let _ = stream2.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream3, _) = listener.accept().unwrap();
    let body3 = read_http_request_body(&mut stream3);
    let _ = stream3.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    let (mut stream4, _) = listener.accept().unwrap();
    let body4 = read_http_request_body(&mut stream4);
    let _ = stream4.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");

    let all_bodies = [body1, body2, body3, body4];

    // Separate crash pings from crash reports
    let mut crash_pings = Vec::new();
    let mut crash_reports = Vec::new();

    for (i, body) in all_bodies.iter().enumerate() {
        if body.contains("is_crash_ping:true") {
            crash_pings.push((i + 1, body));
        } else if body.contains("is_crash:true") {
            crash_reports.push((i + 1, body));
        }
    }

    assert_eq!(
        crash_pings.len(),
        2,
        "Expected 2 crash pings (telemetry + errors intake), got {}",
        crash_pings.len()
    );
    assert_eq!(
        crash_reports.len(),
        2,
        "Expected 2 crash reports (telemetry + errors intake), got {}",
        crash_reports.len()
    );

    // Find telemetry requests
    let telemetry_crash_ping = crash_pings
        .iter()
        .find(|(_, body)| body.contains("api_version") && body.contains("request_type"))
        .expect("Should have telemetry crash ping");
    assert_crash_ping_message(telemetry_crash_ping.1);

    let telemetry_crash_report = crash_reports
        .iter()
        .find(|(_, body)| {
            body.contains("api_version")
                && body.contains("request_type")
                && body.contains("is_crash:true")
        })
        .expect("Should have telemetry crash report");
    assert_telemetry_message(telemetry_crash_report.1.as_bytes(), crash_typ);

    // Find errors intake requests (contain ddsource: crashtracker but no api_version)
    let errors_crash_ping = crash_pings
        .iter()
        .find(|(_, body)| {
            body.contains("\"ddsource\":\"crashtracker\"") && !body.contains("api_version")
        })
        .expect("Should have errors intake crash ping");

    let errors_crash_report = crash_reports
        .iter()
        .find(|(_, body)| {
            body.contains("\"ddsource\":\"crashtracker\"") && !body.contains("api_version")
        })
        .expect("Should have errors intake crash report");

    // Parse and validate errors intake payloads
    let errors_ping_payload: serde_json::Value = serde_json::from_str(errors_crash_ping.1).unwrap();
    let errors_report_payload: serde_json::Value =
        serde_json::from_str(errors_crash_report.1).unwrap();

    // Validate errors intake crash ping (is_crash: false)
    assert_eq!(errors_ping_payload["ddsource"], "crashtracker");
    assert_eq!(errors_ping_payload["error"]["is_crash"], false);

    // Validate errors intake crash report (is_crash: true)
    assert_eq!(errors_report_payload["ddsource"], "crashtracker");
    assert_eq!(errors_report_payload["error"]["is_crash"], true);

    // Clean up
    drop(listener);
    let _ = std::fs::remove_file(socket_path);
}

fn assert_errors_intake_payload(errors_intake_content: &[u8], crash_typ: &str) {
    let payload = serde_json::from_slice::<serde_json::Value>(errors_intake_content)
        .context("deserializing errors intake payload to json")
        .unwrap();

    // Validate basic structure
    assert_eq!(payload["ddsource"], "crashtracker");
    assert!(payload["timestamp"].is_number());
    assert!(payload["ddtags"].is_string());
    assert_os_info_matches(&payload["os_info"], "errors intake payload");

    let ddtags = payload["ddtags"].as_str().unwrap();
    assert!(ddtags.contains("service:foo"));
    assert!(ddtags.contains("uuid:"));

    let error = &payload["error"];
    assert_eq!(error["source_type"], "Crashtracking");
    assert!(error["type"].is_string()); // Note: "error_type" field is serialized as "type"
    assert!(error["message"].is_string());

    // Check if this is a crash ping or crash report
    if ddtags.contains("is_crash_ping:true") {
        assert_eq!(error["is_crash"], false);
        assert!(error["stack"].is_null());
    } else {
        assert_eq!(error["is_crash"], true);
    }

    // Validate sig_info when present
    if payload.get("sig_info").is_some() && payload["sig_info"].is_object() {
        let sig = &payload["sig_info"];
        let expected = match crash_typ {
            "null_deref" | "kill_sigsegv" | "raise_sigsegv" => ("SIGSEGV", libc::SIGSEGV),
            "kill_sigabrt" | "raise_sigabrt" => ("SIGABRT", libc::SIGABRT),
            "kill_sigill" | "raise_sigill" => ("SIGILL", libc::SIGILL),
            "kill_sigbus" | "raise_sigbus" => ("SIGBUS", libc::SIGBUS),
            other => panic!("Unexpected crash_typ: {other}"),
        };
        assert_eq!(
            sig["si_signo"].as_i64().unwrap_or_default(),
            expected.1 as i64
        );
        assert_eq!(
            sig["si_signo_human_readable"].as_str().unwrap_or(""),
            expected.0
        );
    }

    // Check signal-specific values
    match crash_typ {
        "null_deref" => {
            assert_eq!(error["type"], "SIGSEGV");
            assert!(error["message"]
                .as_str()
                .unwrap()
                .contains("Process terminated"));
            assert!(error["message"].as_str().unwrap().contains("SIGSEGV"));
        }
        "kill_sigabrt" | "raise_sigabrt" => {
            assert_eq!(error["type"], "SIGABRT");
            assert!(error["message"].as_str().unwrap().contains("SIGABRT"));
        }
        "kill_sigill" | "raise_sigill" => {
            assert_eq!(error["type"], "SIGILL");
            assert!(error["message"].as_str().unwrap().contains("SIGILL"));
        }
        "kill_sigbus" | "raise_sigbus" => {
            assert_eq!(error["type"], "SIGBUS");
            assert!(error["message"].as_str().unwrap().contains("SIGBUS"));
        }
        "kill_sigsegv" | "raise_sigsegv" => {
            assert_eq!(error["type"], "SIGSEGV");
            assert!(error["message"].as_str().unwrap().contains("SIGSEGV"));
        }
        _ => panic!("Unexpected crash_typ: {crash_typ}"),
    }
}

fn extend_path<T: AsRef<Path>>(parent: &Path, path: T) -> PathBuf {
    let mut parent = parent.to_path_buf();
    parent.push(path);
    parent
}
