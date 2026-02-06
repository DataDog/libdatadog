// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pre-builds all artifacts needed by bin_tests.
//!
//! This binary is intended to be run before the test suite starts (e.g., via nextest's
//! setup script feature). By building all artifacts upfront, individual tests can skip
//! the cargo build step and just use the pre-built artifacts.
//!
//! Run with: cargo run --bin prebuild

use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};

fn main() -> anyhow::Result<()> {
    println!("Pre-building bin_tests artifacts...");

    // Collect all artifacts that tests need
    let artifacts = collect_all_artifacts();
    let artifact_refs: Vec<&ArtifactsBuild> = artifacts.iter().collect();

    // Build all artifacts
    let start = std::time::Instant::now();
    build_artifacts(&artifact_refs)?;
    let elapsed = start.elapsed();

    println!(
        "Successfully pre-built {} artifacts in {:.2}s",
        artifacts.len(),
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// Collects all artifacts that bin_tests need.
/// This includes all combinations of profiles, artifact types, and variants.
fn collect_all_artifacts() -> Vec<ArtifactsBuild> {
    let mut artifacts = Vec::new();

    // Standard artifacts for both Debug and Release profiles
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        // crashtracker_bin_test - used by most crash tracking tests
        artifacts.push(ArtifactsBuild {
            name: "crashtracker_bin_test".to_owned(),
            build_profile: profile,
            artifact_type: ArtifactType::Bin,
            ..Default::default()
        });

        // test_crashtracker_receiver - the receiver binary
        artifacts.push(ArtifactsBuild {
            name: "test_crashtracker_receiver".to_owned(),
            build_profile: profile,
            artifact_type: ArtifactType::Bin,
            ..Default::default()
        });

        // crashing_test_app - used for panic hook tests (non-macOS)
        #[cfg(not(target_os = "macos"))]
        artifacts.push(ArtifactsBuild {
            name: "crashing_test_app".to_owned(),
            build_profile: profile,
            artifact_type: ArtifactType::Bin,
            ..Default::default()
        });

        // test_the_tests - used by test_the_tests.rs
        artifacts.push(ArtifactsBuild {
            name: "test_the_tests".to_owned(),
            build_profile: profile,
            artifact_type: ArtifactType::Bin,
            ..Default::default()
        });

        // libdd-profiling-ffi CDylib - used by test_the_tests.rs
        artifacts.push(ArtifactsBuild {
            name: "libdd-profiling-ffi".to_owned(),
            lib_name_override: Some("datadog_profiling_ffi".to_owned()),
            build_profile: profile,
            artifact_type: ArtifactType::CDylib,
            ..Default::default()
        });
    }

    // Panic abort variants (used by panic hook tests)
    // These are built with -C panic=abort RUSTFLAGS and stored in a separate target directory

    // crashing_test_app with panic_abort - Debug only (used in tests)
    #[cfg(not(target_os = "macos"))]
    artifacts.push(ArtifactsBuild {
        name: "crashing_test_app".to_owned(),
        build_profile: BuildProfile::Debug,
        artifact_type: ArtifactType::Bin,
        panic_abort: Some(true),
        ..Default::default()
    });

    // crashtracker_bin_test with panic_abort - Debug only (used in panic hook tests)
    artifacts.push(ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: BuildProfile::Debug,
        artifact_type: ArtifactType::Bin,
        panic_abort: Some(true),
        ..Default::default()
    });

    artifacts
}