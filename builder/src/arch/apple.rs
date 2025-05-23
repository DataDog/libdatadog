// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use std::ffi::OsStr;

pub const NATIVE_LIBS: &str =
    " -framework Security -framework CoreFoundation -liconv -lSystem -lresolv -lc -lm -liconv";
pub const PROF_DYNAMIC_LIB: &str = "libdatadog_profiling.dylib";
pub const PROF_STATIC_LIB: &str = "libdatadog_profiling.a";
pub const PROF_DYNAMIC_LIB_FFI: &str = "libdatadog_profiling_ffi.dylib";
pub const PROF_STATIC_LIB_FFI: &str = "libdatadog_profiling_ffi.a";
pub const REMOVE_RPATH: bool = true;
pub const BUILD_CRASHTRACKER: bool = true;
pub const RUSTFLAGS: [&str; 2] = ["-C", "relocation-model=pic"];

pub fn fix_rpath(lib_path: &str) {
    if REMOVE_RPATH {
        let lib_name = lib_path.split('/').next_back().unwrap();

        let exit_status = Command::new("install_name_tool")
            .arg("-id")
            .arg("@rpath/".to_string() + lib_name)
            .arg(lib_path)
            .status()
            .expect("Failed to fix rpath using install_name_tool");
        match exit_status.code() {
            Some(0) => {}
            Some(rc) => panic!(
                "Failed to fix rpath using install_name_tool: return code {}",
                rc
            ),
            None => match exit_status.signal() {
                Some(sig) => panic!(
                    "Failed to fix rpath using install_name_tool: killed by signal {}",
                    sig
                ),
                None => panic!(
                    "Failed to fix rpath using install_name_tool: exit status {:?}",
                    exit_status
                ),
            },
        }
    }
}

pub fn strip_libraries(lib_path: &str) {
    // objcopy is not available in macos image. Investigate llvm-objcopy
    let mut strip = Command::new("strip")
        .arg("-S")
        .arg(lib_path.to_owned() + "/libdatadog_profiling.dylib")
        .spawn()
        .expect("Failed to spawn strip");

    strip.wait().expect("Failed to strip library");
}

pub fn fix_soname(_lib_path: &str) {}

pub fn add_additional_files(_lib_path: &str, _target_path: &OsStr) {}
