// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pre-builds all artifacts needed by bin_tests.
//!
//! This binary is intended to be run before the test suite starts (e.g., via nextest's
//! setup script feature). By building all artifacts upfront, individual tests can skip
//! the cargo build step and just use the pre-built artifacts.
//!
//! Run with: cargo run -p bin_tests --bin prebuild

use bin_tests::{artifacts, build_artifacts};

fn main() -> anyhow::Result<()> {
    println!("Pre-building bin_tests artifacts...");

    // Get all artifacts from the shared module
    let artifacts = artifacts::all_prebuild_artifacts();
    let artifact_refs: Vec<_> = artifacts.iter().collect();

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
