// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsStr;
use std::process::Command;

use crate::utils::wait_for_success;

pub const NATIVE_LIBS: &str = " -ldl -lrt -lpthread -lc -lm -lrt -lpthread -lutil -ldl -lutil";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.so";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.so";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const REMOVE_RPATH: bool = false;
pub const BUILD_CRASHTRACKER: bool = true;
pub const RUSTFLAGS: [&str; 4] = [
    "-C",
    "relocation-model=pic",
    "-C",
    "link-arg=-Wl,-soname,libdatadog_profiling.so",
];

pub fn fix_rpath(lib_path: &str) {
    if REMOVE_RPATH {
        let patchelf = Command::new("patchelf")
            .arg("--remove-rpath")
            .arg(lib_path)
            .spawn()
            .expect("failed to spawn patchelf");

        wait_for_success(patchelf, "patchelf");
    }
}

pub fn strip_libraries(lib_path: &str) {
    let rm_section = Command::new("objcopy")
        .arg("--remove-section")
        .arg(".llvmbc")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.a")
        .spawn()
        .expect("failed to spawn objcopy");

    wait_for_success(rm_section, "objcopy (remove llvmbc section)");

    let create_debug = Command::new("objcopy")
        .arg("--only-keep-debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.debug")
        .spawn()
        .expect("Failed to spawn objcopy");

    wait_for_success(create_debug, "objcopy (extract debug info)");

    let strip = Command::new("strip")
        .arg("-S")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("Failed to spawn strip");

    wait_for_success(strip, "strip");

    let debug = Command::new("objcopy")
        .arg("--add-gnu-debuglink=".to_string() + lib_path + "/libdatadog_profiling.debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("Failed to spawn objcopy");

    wait_for_success(debug, "objcopy (set debuglink)");
}

pub fn add_additional_files(_lib_path: &str, _target_path: &OsStr) {}
