// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows integration tests for crash tracking.
//! These tests spawn Windows binaries, trigger crashes, and validate crash reports.

#![cfg(windows)]

use bin_tests::{
    build_artifacts,
    test_runner_windows::{run_windows_crash_test, WindowsCrashTestConfig},
    test_types_windows::{WindowsCrashType, WindowsTestMode},
    validation_windows::WindowsPayloadValidator,
    ArtifactType, ArtifactsBuild, BuildProfile,
};

/// Helper function to build Windows crash tracking artifacts.
fn build_windows_artifacts(profile: BuildProfile) -> anyhow::Result<WindowsArtifacts> {
    let crashtracker_bin = ArtifactsBuild {
        name: "crashtracker_bin_test_windows".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        ..Default::default()
    };

    // Build the WER handler DLL from bin_tests (src/wer_handler.rs)
    // When bin_tests builds as cdylib, it includes the wer_handler module
    let wer_handler_dll = ArtifactsBuild {
        name: "bin_tests".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::CDylib,
        ..Default::default()
    };

    let artifacts_map = build_artifacts(&[&crashtracker_bin, &wer_handler_dll])?;

    Ok(WindowsArtifacts {
        crashtracker_bin: artifacts_map[&crashtracker_bin].clone(),
        wer_handler_dll: artifacts_map[&wer_handler_dll].clone(),
    })
}

struct WindowsArtifacts {
    crashtracker_bin: std::path::PathBuf,
    wer_handler_dll: std::path::PathBuf,
}

/// MVP Test 1: Basic null pointer access violation
#[test]
#[cfg_attr(miri, ignore)]
fn test_windows_crash_null_deref() {
    let config = WindowsCrashTestConfig::new(
        BuildProfile::Debug,
        WindowsTestMode::Basic,
        WindowsCrashType::AccessViolationNull,
    );

    let artifacts = build_windows_artifacts(config.profile).unwrap();

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_handler_dll,
        |payload, _fixtures| {
            WindowsPayloadValidator::new(payload)
                .validate_exception_code(0xC0000005)? // EXCEPTION_ACCESS_VIOLATION
                .validate_stack_exists()?
                .validate_thread_info()?
                .validate_os_info()?
                .validate_metadata()?;
            Ok(())
        },
    )
    .unwrap();
}

/// MVP Test 2: Division by zero
#[test]
#[cfg_attr(miri, ignore)]
fn test_windows_crash_divide_by_zero() {
    let config = WindowsCrashTestConfig::new(
        BuildProfile::Release,
        WindowsTestMode::Basic,
        WindowsCrashType::DivideByZero,
    );

    let artifacts = build_windows_artifacts(config.profile).unwrap();

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_handler_dll,
        |payload, _fixtures| {
            WindowsPayloadValidator::new(payload)
                .validate_exception_code(0xC0000094)? // EXCEPTION_INT_DIVIDE_BY_ZERO
                .validate_stack_exists()?
                .validate_os_info()?;
            Ok(())
        },
    )
    .unwrap();
}

/// MVP Test 3: Stack overflow
#[test]
#[cfg_attr(miri, ignore)]
fn test_windows_crash_stack_overflow() {
    let config = WindowsCrashTestConfig::new(
        BuildProfile::Release,
        WindowsTestMode::Basic,
        WindowsCrashType::StackOverflow,
    );

    let artifacts = build_windows_artifacts(config.profile).unwrap();

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_handler_dll,
        |payload, _fixtures| {
            WindowsPayloadValidator::new(payload)
                .validate_exception_code(0xC00000FD)? // EXCEPTION_STACK_OVERFLOW
                .validate_stack_exists()?
                .allow_incomplete_stack()?; // Stack overflow often has incomplete stacks
            Ok(())
        },
    )
    .unwrap();
}

/// MVP Test 4: Abort/panic
#[test]
#[cfg_attr(miri, ignore)]
fn test_windows_crash_abort() {
    let config = WindowsCrashTestConfig::new(
        BuildProfile::Debug,
        WindowsTestMode::Basic,
        WindowsCrashType::Abort,
    );

    let artifacts = build_windows_artifacts(config.profile).unwrap();

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_handler_dll,
        |payload, _fixtures| {
            WindowsPayloadValidator::new(payload)
                // Using access violation for reliable WER triggering
                // (std::process::abort may not trigger WER in all configurations)
                .validate_exception_code(0xC0000005)? // EXCEPTION_ACCESS_VIOLATION
                .validate_stack_exists()?
                .validate_metadata()?;
            Ok(())
        },
    )
    .unwrap();
}

/// MVP Test 5: Registry key management
#[test]
#[cfg_attr(miri, ignore)]
fn test_windows_registry_management() {
    use bin_tests::test_runner_windows::registry_key_exists;

    // Generate unique registry key for this test
    let registry_key = format!(
        "DatadogTest_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    let config = WindowsCrashTestConfig::new(
        BuildProfile::Debug,
        WindowsTestMode::RegistryTest,
        WindowsCrashType::AccessViolationNull,
    )
    .with_registry_key(registry_key.clone());

    let artifacts = build_windows_artifacts(config.profile).unwrap();

    // Registry key should not exist before test
    let exists_before = registry_key_exists(&registry_key).unwrap_or(false);
    assert!(!exists_before, "Registry key should not exist before test");

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_handler_dll,
        |payload, _fixtures| {
            WindowsPayloadValidator::new(payload)
                .validate_exception_code(0xC0000005)?
                .validate_stack_exists()?;
            Ok(())
        },
    )
    .unwrap();

    // Registry key should be cleaned up after test
    let exists_after = registry_key_exists(&registry_key).unwrap_or(true);
    assert!(
        !exists_after,
        "Registry key should be cleaned up after test"
    );
}

// Additional tests can be added as the implementation matures:
// - test_windows_crash_illegal_instruction
// - test_windows_multithreaded_crash
// - test_windows_deep_stack
// - test_windows_wer_context_validation
// - test_windows_payload_compatibility (cross-platform validation)
