// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Build CXX bridge - cross-platform function
#[cfg(feature = "cxx")]
fn build_cxx_bridge() {
    cxx_build::bridge("src/exporter/cxx.rs")
        .flag_if_supported("-std=c++20")
        .compile("libdd-profiling-cxx");

    println!("cargo:rerun-if-changed=src/exporter/cxx.rs");
}

fn main() {
    // Build CXX bridge if feature is enabled
    #[cfg(feature = "cxx")]
    build_cxx_bridge();
}
