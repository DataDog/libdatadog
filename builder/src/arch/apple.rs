// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsStr;
use std::process::Command;

use crate::utils::wait_for_success;

pub const NATIVE_LIBS: &str =
    " -framework Security -framework CoreFoundation -liconv -lSystem -lresolv -lc -lm -liconv";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.dylib";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.dylib";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const BUILD_CRASHTRACKER: bool = true;
pub const RUSTFLAGS: [&str; 4] = [
    "-C",
    "relocation-model=pic",
    "-C",
    "link-arg=-Wl,-install_name,@rpath/libdatadog_profiling.dylib",
];

pub fn strip_libraries(lib_path: &str) {
    // objcopy is not available in macos image. Investigate llvm-objcopy
    let strip = Command::new("strip")
        .arg("-S")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.dylib")
        .spawn()
        .expect("Failed to spawn strip");

    wait_for_success(strip, "strip");
}

pub fn add_additional_files(_lib_path: &str, _target_path: &OsStr) {}
