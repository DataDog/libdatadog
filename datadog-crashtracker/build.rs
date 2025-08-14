// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
fn build_libtest_so() {
    let base_path = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest");

    let src = base_path.join("libtest.c");
    let dst = base_path.join("libtest.so");
    let mut cc_build = Command::new("cc")
        .arg(src)
        .arg("-shared")
        .arg("-fPIC")
        // this is needed for the cross compile (cargo cross)
        .arg("-std=c99")
        .arg("-Wl,--version-script,data/libtest.map")
        .arg("-O0")
        .arg("-gdwarf-4")
        // Fix build id to ease in testing.
        .arg("-Wl,--build-id=0xac33885879e4d40850d3d0fd68a1ac8e0d799dee")
        .arg("-o")
        .arg(dst)
        .spawn()
        .expect("failed to spawn cc command");

    cc_build.wait().expect("failed to build libtest.so");
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
