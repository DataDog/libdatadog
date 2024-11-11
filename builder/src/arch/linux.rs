// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

use std::ffi::OsStr;

pub const NATIVE_LIBS: &str = " -ldl -lrt -lpthread -lc -lm -lrt -lpthread -lutil -ldl -lutil";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.so";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.so";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const REMOVE_RPATH: bool = false;
pub const BUILD_CRASHTRACKER: bool = true;
pub const RUSTFLAGS: [&str; 2] = ["-C", "relocation-model=pic"];

pub fn fix_rpath(lib_path: &str) {
    if REMOVE_RPATH {
        let mut patchelf = Command::new("patchelf")
            .arg("--remove-rpath")
            .arg(lib_path)
            .spawn()
            .expect("failed to spawn patchelf");

        patchelf.wait().expect("failed to remove rpath");
    }
}

pub fn strip_libraries(lib_path: &str) {
    let mut rm_section = Command::new("objcopy")
        .arg("--remove-section")
        .arg(".llvmbc")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.a")
        .spawn()
        .expect("failed to spawn objcopy");

    rm_section.wait().expect("Failed to remove llvmbc section");

    let mut create_debug = Command::new("objcopy")
        .arg("--only-keep-debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.debug")
        .spawn()
        .expect("Failed to spawn objcopy");

    create_debug.wait().expect("Failed to extract debug info");

    let mut strip = Command::new("strip")
        .arg("-S")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("Failed to spawn strip");

    strip.wait().expect("Failed to strip library");

    let mut debug = Command::new("objcopy")
        .arg("--add-gnu-debuglink=".to_string() + lib_path + "/libdatadog_profiling.debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("Failed to spawn objcopy");

    debug.wait().expect("Failed to set debuglink");
}

pub fn fix_soname(lib_path: &str) {
    let mut patch_soname = Command::new("patchelf")
        .arg("--set-soname")
        .arg(PROF_DYNAMIC_LIB)
        .arg(lib_path.to_owned() + "/" + PROF_DYNAMIC_LIB)
        .spawn()
        .expect("failed to span patchelf");

    patch_soname.wait().expect("failed to change the soname");
}

pub fn add_additional_files(_lib_path: &str, _target_path: &OsStr) {}
