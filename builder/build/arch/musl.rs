// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

pub const NATIVE_LIBS: &str = " -lssp_nonshared -lc";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.so";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.so";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const REMOVE_RPATH: bool = true;
pub const BUILD_CRASHTRACKER: bool = true;

pub fn fix_rpath(lib_path: &str) {
    if REMOVE_RPATH {
        Command::new("patchelf")
            .arg("--remove-rpath")
            .arg(lib_path)
            .spawn()
            .expect("failed to remove rpath");
    }
}

pub fn strip_libraries(lib_path: &str) {
    command::new("objcopy")
        .arg("--remove-section")
        .arg(".llvmbc")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.a")
        .spawn()
        .expect("failed to remove llvm section");

    command::new("objcopy")
        .arg("--only-keep-debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.debug")
        .spawn()
        .expect("failed to create debug file");

    command::new("strip")
        .arg("-s")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("failed to strip the library");

    command::new("objcopy")
        .arg("--add-gnu-debuglink=".to_string() + lib_path + "/libdatadog_profiling.debug")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.so")
        .spawn()
        .expect("failed to create debug file");
}
