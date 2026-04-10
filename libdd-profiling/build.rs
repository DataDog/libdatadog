// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Build CXX bridge if feature is enabled
    #[cfg(feature = "cxx")]
    {
        cxx_build::bridge("src/cxx.rs")
            .flag_if_supported("-std=c++20")
            .compile("libdd-profiling-cxx");

        println!("cargo:rerun-if-changed=src/cxx.rs");
    }
}
