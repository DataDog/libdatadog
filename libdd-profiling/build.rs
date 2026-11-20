// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Test-only: `LIBDD_ALLOCATION_CHAOS_TEST=1` wraps the profiling arenas for the
    // allocation-failure harness of libdd-profiling-ffi. A cfg rather than a Cargo feature, so
    // feature unification can never turn it on in a shipped build.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LIBDD_ALLOCATION_CHAOS_TEST");
    println!("cargo:rustc-check-cfg=cfg(allocation_chaos_test)");
    if std::env::var_os("LIBDD_ALLOCATION_CHAOS_TEST").is_some_and(|v| v == "1") {
        println!("cargo:rustc-cfg=allocation_chaos_test");
    }

    // Build CXX bridge if feature is enabled
    #[cfg(feature = "cxx")]
    {
        cxx_build::bridge("src/cxx.rs")
            .flag_if_supported("-std=c++20")
            .compile("libdd-profiling-cxx");

        let out_dir =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        let include_dir = out_dir.join("cxxbridge/include/datadog");
        std::fs::create_dir_all(&include_dir).expect("create CXX convenience include directory");
        std::fs::copy(
            "include/datadog/profiling.hpp",
            include_dir.join("profiling.hpp"),
        )
        .expect("copy CXX profiling convenience header");

        println!("cargo:rerun-if-changed=src/cxx.rs");
        println!("cargo:rerun-if-changed=include/datadog/profiling.hpp");
    }
}
