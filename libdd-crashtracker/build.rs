// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(unix)]
mod unix_imports {
    pub use std::path::Path;
    pub use std::path::PathBuf;
    pub use std::process::Command;
}

#[cfg(unix)]
use unix_imports::*;

pub use libdd_common::cc_utils::cc;

// Build CXX bridge - cross-platform function
#[cfg(feature = "cxx")]
fn build_cxx_bridge() {
    let mut build = cxx_build::bridge("src/crash_info/cxx.rs");
    build.flag_if_supported("-std=c++20");

    // On Windows, use dynamic CRT (/MD) to match the default Rust build
    #[cfg(target_os = "windows")]
    build.static_crt(false);

    build.compile("libdd-crashtracker-cxx");

    println!("cargo:rerun-if-changed=src/crash_info/cxx.rs");
}

#[cfg(unix)]
fn build_shared_libs() {
    build_c_file();
    build_cpp_file();
}

#[cfg(unix)]
fn get_tests_folder_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest")
}

#[cfg(unix)]
fn build_c_file() {
    let base_path = get_tests_folder_path();

    let src = base_path.join("libtest.c");
    let dst = base_path.join("libtest.so");
    let dst_file = dst
        .to_str()
        .expect("Failed to convert dst file path to str");

    println!("cargo:rerun-if-changed={}", &dst_file);
    libdd_common::cc_utils::ImprovedBuild::new()
        .file(&src)
        .link_dynamically("dl")
        // this is needed for the cross compile (cargo cross)
        .flag("-std=c99")
        // Fix build id to ease in testing.
        .flag("-Wl,--build-id=0xaaaabbbbccccddddeeeeffff0011223344556677")
        .flag("-O0")
        .flag("-gdwarf-4")
        .flag("-Wl,--compress-debug-sections=zlib")
        .warnings(true)
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true)
        .try_compile_shared_lib(dst_file)
        .unwrap();

    // We use objcopy to change the alignment of the debug_abbrev ELF section.
    // By setting the alignment to 1 we make sure that the section is misaligned.
    // This will help to identify regressions in blazesym.
    // Note: we could have picked any other debug_xx sections. As long as it's a
    // debug ELF sections.
    let mut modify_alignment = Command::new("objcopy")
        .args(["--set-section-alignment", ".debug_abbrev=1"])
        .arg(dst_file)
        .arg(dst_file)
        .spawn()
        .expect("failed to spawn objcopy");

    modify_alignment
        .wait()
        .expect("Failed to change alignement of debug_abbrev ELF section");
}

#[cfg(unix)]
fn build_cpp_file() {
    let base_path = get_tests_folder_path();

    let src = base_path.join("libtest_cpp.cpp");
    let dst = base_path.join("libtest_cpp.so");
    let dst_file = dst
        .to_str()
        .expect("Failed to convert dst file path to str");

    println!("cargo:rerun-if-changed={}", &dst_file);
    libdd_common::cc_utils::ImprovedBuild::new()
        .cpp(true)
        .file(&src)
        .link_dynamically("dl")
        .flag("-std=c++11")
        // Fix build id to ease in testing.
        .flag("-Wl,--build-id=0x0011223344556677aaaabbbbccccddddeeeeffff")
        .flag("-O0")
        .flag("-gdwarf-4")
        .warnings(true)
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true)
        .try_compile_shared_lib(dst_file)
        .unwrap();
}

#[cfg(unix)]
fn main() {
    cc::Build::new()
        .file("src/crash_info/emit_sicodes.c")
        .compile("emit_sicodes");

    // Build CXX bridge if feature is enabled
    #[cfg(feature = "cxx")]
    build_cxx_bridge();

    // Don't build test libraries during `cargo publish` verification.
    // During verification, the package is unpacked to target/package/ and built there.
    let is_packaging = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_default()
        .contains("/target/package/");

    if cfg!(all(
        feature = "generate-unit-test-files",
        not(target_os = "macos")
    )) && !is_packaging
    {
        build_shared_libs();
    }
}

#[cfg(not(unix))]
fn main() {
    // Build CXX bridge if feature is enabled
    #[cfg(feature = "cxx")]
    build_cxx_bridge();
}
