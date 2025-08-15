// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(unix)]
use std::path::Path;

pub use cc_utils::cc;

#[cfg(unix)]
fn build_libtest_so() {
    let base_path = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest");

    let src = base_path.join("libtest.c");
    let dst = base_path.join("libtest.so");

    cc_utils::ImprovedBuild::new()
        .file(src)
        .link_dynamically("dl")
        // this is needed for the cross compile (cargo cross)
        .flag("-std=c99")
        .flag("-Wl,--version-script,data/libtest.map")
        // Fix build id to ease in testing.
        .flag("-Wl,--build-id=0xac33885879e4d40850d3d0fd68a1ac8e0d799dee")
        .flag("-O0")
        .flag("-gdwarf-4")
        .warnings(true)
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true)
        .try_compile_shared_lib(dst.to_str().expect("Failed to convert path to str"))
        .unwrap();
}

#[cfg(unix)]
fn main() {
    cc::Build::new()
        .file("src/crash_info/emit_sicodes.c")
        .compile("emit_sicodes");
    if cfg!(feature = "generate-unit-test-files") {
        build_libtest_so();
    }
}

#[cfg(not(unix))]
fn main() {}
