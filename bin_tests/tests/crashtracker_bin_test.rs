// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};
use serde_json::Value;

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_debug() {
    test_crash_tracking_bin(BuildProfile::Debug, "donothing", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigpipe() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigpipe", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigchld", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld_exec() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigchld_exec", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "donothing_sigstack", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigpipe_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "sigpipe_sigstack", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "sigchld_sigstack", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_chained() {
    test_crash_tracking_bin(BuildProfile::Release, "chained", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_fork() {
    test_crash_tracking_bin(BuildProfile::Release, "fork", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_kill_sigabrt() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "kill_sigabrt");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_kill_sigill() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "kill_sigill");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_kill_sigbus() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "kill_sigbus");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_kill_sigsegv() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "kill_sigsegv");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_raise_sigabrt() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "raise_sigabrt");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_raise_sigill() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "raise_sigill");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_raise_sigbus() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "raise_sigbus");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_raise_sigsegv() {
    // For now, do the base test (donothing).  For future we should probably also test chaining.
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "raise_sigsegv");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_prechain_sigabrt() {
    test_crash_tracking_bin(BuildProfile::Release, "prechain_abort", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_frame() {
    test_crash_tracking_bin_runtime_callback_frame_impl(
        BuildProfile::Release,
        "runtime_callback_frame",
        "null_deref",
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_string() {
    test_crash_tracking_bin_runtime_callback_string_impl(
        BuildProfile::Release,
        "runtime_callback_string",
        "null_deref",
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_no_runtime_callback() {
    test_crash_tracking_bin_no_runtime_callback_impl(
        BuildProfile::Release,
        "donothing",
        "null_deref",
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_runtime_callback_frame_invalid_utf8() {
    test_crash_tracking_bin_runtime_callback_frame_invalid_utf8_impl(
        BuildProfile::Release,
        "runtime_callback_frame_invalid_utf8",
        "null_deref",
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_ping_timing_and_content() {
    test_crash_tracking_bin(BuildProfile::Release, "donothing", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_errors_intake_upload() {
    test_crash_tracking_bin_with_errors_intake(BuildProfile::Release, "donothing", "null_deref");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_errors_intake_crash_ping() {
    test_crash_tracking_errors_intake_dual_upload(BuildProfile::Release, "donothing", "null_deref");
}

// This test is disabled for now on x86_64 musl and macos
// It seems that on aarch64 musl, libc has CFI which allows
// unwinding passed the signal frame.
#[test]
#[cfg(not(any(all(target_arch = "x86_64", target_env = "musl"), target_os = "macos")))]
#[cfg_attr(miri, ignore)]
fn test_crasht_tracking_validate_callstack() {
    test_crash_tracking_callstack()
}

#[test]
#[cfg(not(any(all(target_arch = "x86_64", target_env = "musl"), target_os = "macos")))]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_callstack() {
    let (_, crashtracker_receiver) = setup_crashtracking_crates(BuildProfile::Release);

    let crashing_app = ArtifactsBuild {
        name: "crashing_test_app".to_owned(),
        // compile in debug so we avoid inlining
        // and can check the callchain
        build_profile: BuildProfile::Debug,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        ..Default::default()
    };

    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashing_app]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashing_app])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });
    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    let crash_profile = fs::read(fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Note: in Release, we do not have the crate and module name prepended to the function name
    // Here we compile the crashing app in Debug.
    let expected_functions = [
        "crashing_test_app::unix::fn3",
        "crashing_test_app::unix::fn2",
        "crashing_test_app::unix::fn1",
        "crashing_test_app::unix::main",
        "crashing_test_app::main",
    ];

    let crashing_callstack = &crash_payload["error"]["stack"]["frames"];
    assert!(
        crashing_callstack.as_array().unwrap().len() >= expected_functions.len(),
        "crashing thread callstacks does have less frames than expected. Current: {}, Expected: {}",
        crashing_callstack.as_array().unwrap().len(),
        expected_functions.len()
    );

    let function_names: Vec<&str> = crashing_callstack
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["function"].as_str().unwrap_or(""))
        .collect();

    for (expected, actual) in expected_functions.iter().zip(function_names.iter()) {
        assert_eq!(expected, actual);
    }
}

fn test_crash_tracking_bin_runtime_callback_frame_impl(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(&fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Validate normal crash data first
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );

    let sig_info = &crash_payload["sig_info"];
    assert_siginfo_message(sig_info, crash_typ);

    let error = &crash_payload["error"];
    assert_error_message(&error["message"], sig_info);

    // Validate runtime callback frame data
    validate_runtime_callback_frame_data(&crash_payload);

    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_typ);
        }
    }
}

fn test_crash_tracking_bin_runtime_callback_frame_invalid_utf8_impl(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(&fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Validate normal crash data first
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );

    let sig_info = &crash_payload["sig_info"];
    assert_siginfo_message(sig_info, crash_typ);

    let error = &crash_payload["error"];
    assert_error_message(&error["message"], sig_info);

    // Validate runtime callback frame data with invalid UTF-8
    validate_runtime_callback_frame_invalid_utf8_data(&crash_payload);

    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_typ);
        }
    }
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

fn test_crash_tracking_bin_runtime_callback_string_impl(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    // Runtime callback tests should crash like normal tests
    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(&fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Validate normal crash data first
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );

    let sig_info = &crash_payload["sig_info"];
    assert_siginfo_message(sig_info, crash_typ);

    let error = &crash_payload["error"];
    assert_error_message(&error["message"], sig_info);

    // Validate runtime callback string data
    validate_runtime_callback_string_data(&crash_payload);

    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_typ);
        }
    }
}

fn test_crash_tracking_bin_no_runtime_callback_impl(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    // Should crash like normal tests
    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(&fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Validate normal crash data first
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );

    let sig_info = &crash_payload["sig_info"];
    assert_siginfo_message(sig_info, crash_typ);

    let error = &crash_payload["error"];
    assert_error_message(&error["message"], sig_info);

    // Validate no runtime callback data is present
    validate_no_runtime_callback_data(&crash_payload);

    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_typ);
        }
    }
}

fn test_crash_tracking_bin(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
        .spawn()
        .unwrap();
    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });

    // When we raise SIGSEGV/SIGBUS, the chained handler doesn't kill the program
    // Presumably because continuing after raise is allowed.
    // Not sure why sigill behaves differently??
    // TODO: figure that out.
    match crash_typ {
        "kill_sigabrt" | "kill_sigill" | "null_deref" | "raise_sigabrt" | "raise_sigill" => {
            assert!(!exit_status.success())
        }
        "kill_sigbus" | "kill_sigsegv" | "raise_sigbus" | "raise_sigsegv" => {
            assert!(exit_status.success())
        }
        _ => unreachable!("{crash_typ} shouldn't happen"),
    }

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(&fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );
    let sig_info = &crash_payload["sig_info"];
    assert_siginfo_message(sig_info, crash_typ);

    let error = &crash_payload["error"];
    assert_error_message(&error["message"], sig_info);

    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let payloads = crash_telemetry.split(|&b| b == b'\n').collect::<Vec<_>>();
    for payload in payloads {
        if String::from_utf8_lossy(payload).contains("is_crash:true") {
            assert_telemetry_message(payload, crash_typ);
        }
    }

    // Crashtracking signal handler chaining tests, as well as other tests, might only be able to
    // influence system state after the main application has crashed, and has therefore lost the
    // ability to influence the outcome of the test.  Those tests should create an "INVALID" file
    // in the output directory.
    // - If the file exists and contains only a single 'O' character, the test passes
    // - Likewise, if the file does not exist, the test passes
    // - Tests are free to output additional information in the file in case of a failure; it'll be
    //   read here
    let invalid_path = format!("{0}/INVALID", fixtures.output_dir.display());
    if let Ok(invalid) = fs::read(invalid_path) {
        assert_eq!(invalid, b"O");
    }
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

fn assert_telemetry_message(crash_telemetry: &[u8], crash_typ: &str) {
    // Split by newline and take the first line
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
    assert_eq!(telemetry_payload["payload"].as_array().unwrap().len(), 1);

    let tags = telemetry_payload["payload"][0]["tags"]
        .as_str()
        .unwrap()
        .split(',')
        .filter(|t| !t.starts_with("uuid:"))
        .collect::<std::collections::HashSet<_>>();

    let base_expected_tags: std::collections::HashSet<&str> =
        std::collections::HashSet::from_iter([
            "data_schema_version:1.4",
            // "incomplete:false", // TODO: re-add after fixing musl unwinding
            "is_crash:true",
            "profiler_collecting_sample:1",
            "profiler_inactive:0",
            "profiler_serializing:0",
            "profiler_unwinding:0",
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

    assert_eq!(telemetry_payload["payload"][0]["is_sensitive"], true);
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

    process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        // empty url, endpoint will be set to none
        .arg("")
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg("donothing")
        .arg("null_deref")
        .env(
            "DD_TRACE_AGENT_URL",
            format!("unix://{}", socket_path.display()),
        )
        .spawn()
        .unwrap();

    let (mut stream1, _) = listener.accept().unwrap();
    let body1 = read_http_request_body(&mut stream1);

    stream1
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();

    let (mut stream2, _) = listener.accept().unwrap();
    let body2 = read_http_request_body(&mut stream2);

    stream2
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();

    // We expect up to 4 requests total (crash ping + crash report, each to telemetry + errors intake)
    // Wait for 2 additional requests
    let mut additional_bodies = Vec::new();
    for _ in 3..=4 {
        if let Ok((mut stream, _)) = listener.accept() {
            let body = read_http_request_body(&mut stream);
            additional_bodies.push(body);
            // Send 200 OK response
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        } else {
            break;
        }
    }

    // Collect all requests (now expecting 4: 2 crash pings + 2 crash reports due to dual upload)
    let mut all_bodies = vec![body1, body2];
    all_bodies.extend(additional_bodies);

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
    validate_crash_ping_telemetry(telemetry_crash_ping.1);

    let telemetry_crash_report = crash_reports
        .iter()
        .find(|(_, body)| body.contains("api_version") && body.contains("request_type"))
        .expect("Should have telemetry crash report");
    assert_telemetry_message(telemetry_crash_report.1.as_bytes(), "null_deref");
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
    assert_eq!(telemetry_payload["payload"].as_array().unwrap().len(), 1);

    let log_entry = &telemetry_payload["payload"][0];

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
    let crashtracker_bin = ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: crash_tracking_receiver_profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        ..Default::default()
    };
    let crashtracker_receiver = ArtifactsBuild {
        name: "test_crashtracker_receiver".to_owned(),
        build_profile: crash_tracking_receiver_profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
        ..Default::default()
    };
    (crashtracker_bin, crashtracker_receiver)
}

fn test_crash_tracking_bin_with_errors_intake(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
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

    // Check that errors intake file was created
    let errors_intake_path = fixtures.crash_profile_path.with_extension("errors");
    assert!(
        errors_intake_path.exists(),
        "Errors intake file should be created at {}",
        errors_intake_path.display()
    );

    // Read and validate errors intake payload
    let errors_intake_content = fs::read(&errors_intake_path)
        .context("reading errors intake payload")
        .unwrap();
    let errors_payload = serde_json::from_slice::<serde_json::Value>(&errors_intake_content)
        .context("deserializing errors intake payload to json")
        .unwrap();

    // Validate errors intake payload structure
    assert_errors_intake_payload(&errors_payload, crash_typ);

    // Also validate telemetry still works (dual upload)
    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    assert_telemetry_message(&crash_telemetry, crash_typ);
}

fn test_crash_tracking_errors_intake_dual_upload(
    crash_tracking_receiver_profile: BuildProfile,
    mode: &str,
    crash_typ: &str,
) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
        .arg(crash_typ)
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

    // Check that errors intake file was created
    let errors_intake_path = fixtures.crash_profile_path.with_extension("errors");
    assert!(
        errors_intake_path.exists(),
        "Errors intake file should be created at {}",
        errors_intake_path.display()
    );

    // Read and validate errors intake payload
    let errors_intake_content = fs::read(&errors_intake_path)
        .context("reading errors intake payload")
        .unwrap();

    // The errors intake might contain multiple JSON objects (crash ping + crash report)
    // Try to parse as a single JSON first, if that fails, try line by line
    if let Ok(single_payload) = serde_json::from_slice::<serde_json::Value>(&errors_intake_content)
    {
        // Single JSON payload - validate it
        assert_errors_intake_payload(&single_payload, crash_typ);
    } else {
        // Multiple JSON objects - parse line by line
        let content_str = String::from_utf8(errors_intake_content).unwrap();
        let lines: Vec<&str> = content_str.lines().collect();
        assert!(!lines.is_empty(), "Errors intake file should not be empty");

        let mut _found_crash_ping = false;
        let mut found_crash_report = false;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }

            let payload: serde_json::Value = serde_json::from_str(line)
                .context("parsing errors intake payload line")
                .unwrap();

            assert_errors_intake_payload(&payload, crash_typ);

            // Check which type this is
            let ddtags = payload["ddtags"].as_str().unwrap();
            if ddtags.contains("is_crash_ping:true") {
                _found_crash_ping = true;
            } else {
                found_crash_report = true;
            }
        }

        // In dual upload mode, we expect at least the crash report
        // Crash ping might not always be sent (e.g., file endpoints skip it)
        assert!(
            found_crash_report,
            "Should have found crash report in errors intake"
        );
    }

    // Also validate telemetry still works (dual upload)
    let crash_telemetry = fs::read(&fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    assert_telemetry_message(&crash_telemetry, crash_typ);
}

fn assert_errors_intake_payload(payload: &Value, crash_typ: &str) {
    // Validate basic structure
    assert_eq!(payload["ddsource"], "crashtracker");
    assert!(payload["timestamp"].is_number());
    assert!(payload["ddtags"].is_string());

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
