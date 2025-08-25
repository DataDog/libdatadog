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

pub use cc_utils::cc;

#[cfg(unix)]
fn build_shared_libs() {
    build_c_file();
    build_cpp_file();
}

#[cfg(unix)]
fn get_data_folder_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest")
}

#[cfg(unix)]
fn build_c_file() {
    let base_path = get_data_folder_path();

    let src = base_path.join("libtest.c");
    let dst = base_path.join("libtest.so");
    let dst_file = dst
        .to_str()
        .expect("Failed to convert dst file path to str");

    println!("cargo:rerun-if-changed={}", &dst_file);
    cc_utils::ImprovedBuild::new()
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
    let base_path = get_data_folder_path();

    let src = base_path.join("libtest_cpp.cpp");
    let dst = base_path.join("libtest_cpp.so");
    let dst_file = dst
        .to_str()
        .expect("Failed to convert dst file path to str");

    println!("cargo:rerun-if-changed={}", &dst_file);
    cc_utils::ImprovedBuild::new()
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
    if cfg!(all(
        feature = "generate-unit-test-files",
        not(target_os = "macos")
    )) {
        build_shared_libs();
    }
}

#[cfg(not(unix))]
fn main() {}
