// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

pub const NATIVE_LIBS: &str =
    " -framework Security -framework CoreFoundation -liconv -lSystem -lresolv -lc -lm -liconv";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.dylib";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.dylib";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const REMOVE_RPATH: bool = true;
pub const BUILD_CRASHTRACKER: bool = true;

pub fn fix_rpath(lib_path: &str) {
    if REMOVE_RPATH {
        let lib_name = lib_path.split("/").last().unwrap();

        Command::new("install_name_tool")
            .arg("-id")
            .arg("@rpath/".to_string() + lib_name)
            .arg(lib_path)
            .spawn()
            .expect("Failed to fix rpath");
    }
}

pub fn strip_libraries(lib_path: &str) {
    // objcopy is not available in macos image. Investigate llvm-objcopy
    Command::new("strip")
        .arg("-S")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.dylib")
        .spawn()
        .expect("failed to strip the library");
}
