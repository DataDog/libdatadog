// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(feature = "generate-unit-test-files")]
use std::path::{Path, PathBuf};
#[cfg(feature = "generate-unit-test-files")]
use std::process::Command;

// Build CXX bridge - cross-platform function
#[cfg(feature = "cxx")]
fn build_cxx_bridge(target_os: &str) {
    let mut build = cxx_build::bridge("src/crash_info/cxx.rs");
    build.flag_if_supported("-std=c++20");

    // On Windows, use dynamic CRT (/MD) to match the default Rust build
    if target_os == "windows" {
        build.static_crt(false);
    }

    build.compile("libdd-crashtracker-cxx");

    println!("cargo:rerun-if-changed=src/crash_info/cxx.rs");
}

/// The fixtures go to `OUT_DIR`, never the source tree: a file this script
/// writes and also watches (or that another profile's run of this script
/// rewrites) dirties the crate on the next cargo invocation.
#[cfg(feature = "generate-unit-test-files")]
fn build_shared_libs() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    build_c_file(&out_dir);
    build_cpp_file(&out_dir);
}

#[cfg(feature = "generate-unit-test-files")]
fn get_tests_folder_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest")
}

/// Links `src` into a shared library at `dst`, linking against libdl.
///
/// `cc::Build::compile` only produces static libraries, so test fixtures that
/// must be `.so` files invoke the detected compiler directly.
#[cfg(feature = "generate-unit-test-files")]
fn compile_shared_lib(src: &Path, dst: &Path, cpp: bool, flags: &[&str]) {
    let mut build = cc::Build::new();
    if cpp {
        build.cpp(true);
    }
    for flag in flags {
        build.flag(flag);
    }
    build
        .warnings(true)
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true);

    println!("cargo:rerun-if-changed={}", src.display());
    let compiler = build.try_get_compiler().expect("no C/C++ compiler found");

    let mut cmd = compiler.to_command();
    if !compiler.is_like_msvc() {
        cmd.arg("-shared");
    }
    cmd.arg("-o").arg(dst).arg(src).arg("-ldl");
    println!("compiling: {cmd:?}");

    let status = cmd
        .spawn()
        .expect("failed to spawn compiler")
        .wait()
        .expect("failed to wait for compiler");
    assert!(status.success(), "compilation failed for {}", dst.display());
}

#[cfg(feature = "generate-unit-test-files")]
fn build_c_file(out_dir: &Path) {
    let src = get_tests_folder_path().join("libtest.c");
    let dst = out_dir.join("libtest.so");

    // this is needed for the cross compile (cargo cross)
    // Fix build id to ease in testing.
    compile_shared_lib(
        &src,
        &dst,
        false,
        &[
            "-std=c99",
            "-Wl,--build-id=0xaaaabbbbccccddddeeeeffff0011223344556677",
            "-O0",
            "-gdwarf-4",
            "-Wl,--compress-debug-sections=zlib",
        ],
    );

    // We use objcopy to change the alignment of the debug_abbrev ELF section.
    // By setting the alignment to 1 we make sure that the section is misaligned.
    // This will help to identify regressions in blazesym.
    // Note: we could have picked any other debug_xx sections. As long as it's a
    // debug ELF sections.
    let mut modify_alignment = Command::new("objcopy")
        .args(["--set-section-alignment", ".debug_abbrev=1"])
        .arg(&dst)
        .arg(&dst)
        .spawn()
        .expect("failed to spawn objcopy");

    modify_alignment
        .wait()
        .expect("Failed to change alignement of debug_abbrev ELF section");
}

#[cfg(feature = "generate-unit-test-files")]
fn build_cpp_file(out_dir: &Path) {
    let src = get_tests_folder_path().join("libtest_cpp.cpp");
    let dst = out_dir.join("libtest_cpp.so");

    // Fix build id to ease in testing.
    compile_shared_lib(
        &src,
        &dst,
        true,
        &[
            "-std=c++11",
            "-Wl,--build-id=0x0011223344556677aaaabbbbccccddddeeeeffff",
            "-O0",
            "-gdwarf-4",
        ],
    );
}

fn main() {
    #[cfg(any(feature = "cxx", feature = "generate-unit-test-files"))]
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();

    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );

    #[cfg(feature = "cxx")]
    build_cxx_bridge(&target_os);

    if target_family != "unix" {
        return;
    }

    println!("cargo:rerun-if-changed=src/crash_info/emit_sicodes.c");
    cc::Build::new()
        .file("src/crash_info/emit_sicodes.c")
        .compile("emit_sicodes");

    // Don't build test libraries during `cargo publish` verification.
    // During verification, the package is unpacked to target/package/ and built there.
    #[cfg(feature = "generate-unit-test-files")]
    {
        let is_packaging = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_default()
            .contains("/target/package/");

        if target_os != "macos" && !is_packaging {
            build_shared_libs();
        }
    }
}
