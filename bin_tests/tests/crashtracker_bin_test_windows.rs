// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows integration tests for crash tracking.
//! These tests spawn Windows binaries, trigger crashes, and validate crash reports.

#![cfg(windows)]

use anyhow::Context;
use bin_tests::{
    build_artifacts,
    test_runner_windows::{run_windows_crash_test, WindowsCrashTestConfig},
    test_types_windows::WindowsCrashType,
    validation_windows::WindowsPayloadValidator,
    ArtifactType, ArtifactsBuild, BuildProfile,
};
use libdd_crashtracker::ExceptionCode;

/// Macro to generate Windows crash tracking tests.
/// This replaces repetitive test functions with a single declaration.
/// All tests are compiled in Release mode for consistency.
macro_rules! windows_crash_tests {
    ($(($test_name:ident, $crash_type:expr, $exception_code:expr)),* $(,)?) => {
        $(
            #[test]
            #[cfg_attr(miri, ignore)]
            fn $test_name() {
                run_standard_windows_crash_test($crash_type, $exception_code);
            }
        )*
    };
}

// Generate all standard Windows crash tracking tests (Release mode)
windows_crash_tests! {
    (test_windows_crash_null_deref, WindowsCrashType::AccessViolationNull, ExceptionCode::AccessViolation),
    (test_windows_crash_divide_by_zero, WindowsCrashType::DivideByZero, ExceptionCode::IntDivideByZero),
    (test_windows_crash_abort, WindowsCrashType::Abort, ExceptionCode::AccessViolation),
    (test_windows_crash_stack_overflow, WindowsCrashType::StackOverflow, ExceptionCode::StackOverflow),
    (test_windows_crash_illegal_instruction, WindowsCrashType::IllegalInstruction, ExceptionCode::IllegalInstruction),
    (test_windows_crash_read_violation, WindowsCrashType::AccessViolationRead, ExceptionCode::AccessViolation),
    (test_windows_crash_write_violation, WindowsCrashType::AccessViolationWrite, ExceptionCode::AccessViolation),
}

/// Standard Windows crash test runner using the refactored infrastructure.
/// This eliminates repetitive test code by providing a common validation pipeline.
/// All tests are compiled in Release mode for consistency and performance.
fn run_standard_windows_crash_test(crash_type: WindowsCrashType, exception_code: ExceptionCode) {
    let config = WindowsCrashTestConfig::new(BuildProfile::Release, crash_type);
    let artifacts = build_windows_artifacts(config.profile).unwrap();

    run_windows_crash_test(
        &config,
        &artifacts.crashtracker_bin,
        &artifacts.wer_simulator,
        move |payload, _fixtures| {
            // Standard comprehensive validation chain
            WindowsPayloadValidator::new(payload)
                .and_then(|v| v.validate_uuid_present())
                .and_then(|v| v.validate_data_schema_version())
                .and_then(|v| v.validate_is_crash_report())
                .and_then(|v| v.validate_error_kind_is_panic())
                .and_then(|v| v.validate_source_type())
                .and_then(|v| v.validate_report_is_complete())
                .and_then(|v| v.validate_incomplete_stack(false))
                .and_then(|v| v.validate_error_message(exception_code))
                .and_then(|v| v.validate_error_stack_exists())
                .and_then(|v| v.validate_threads())
                .and_then(|v| v.validate_os_info())
                .and_then(|v| v.validate_metadata())
                .and_then(|v| v.validate_timestamp())
                .with_context(|| {
                    format!(
                        "Validation failed. Full payload:\n{}",
                        serde_json::to_string_pretty(payload)
                            .unwrap_or_else(|_| "Unable to serialize payload".to_string())
                    )
                })?;
            Ok(())
        },
    )
    .unwrap();
}

/// Helper function to build Windows crash tracking artifacts.
fn build_windows_artifacts(profile: BuildProfile) -> anyhow::Result<WindowsArtifacts> {
    let crashtracker_bin = ArtifactsBuild {
        name: "crashtracker_bin_test_windows".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        ..Default::default()
    };

    // Build the WER simulator binary (out-of-process crash handler)
    let wer_simulator = ArtifactsBuild {
        name: "wer_simulator".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        ..Default::default()
    };

    let artifacts_map = build_artifacts(&[&crashtracker_bin, &wer_simulator])?;

    Ok(WindowsArtifacts {
        crashtracker_bin: artifacts_map[&crashtracker_bin].clone(),
        wer_simulator: artifacts_map[&wer_simulator].clone(),
    })
}

struct WindowsArtifacts {
    crashtracker_bin: std::path::PathBuf,
    wer_simulator: std::path::PathBuf,
}
