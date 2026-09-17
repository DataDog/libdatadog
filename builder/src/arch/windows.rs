// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

pub const NATIVE_LIBS: &str = "";
pub const PROF_DYNAMIC_LIB: &str = "datadog_profiling_ffi.dll";
pub const PROF_STATIC_LIB: &str = "datadog_profiling_ffi.lib";
pub const PROF_PDB: &str = "datadog_profiling_ffi.pdb";
pub const PROF_DYNAMIC_LIB_FFI: &str = "datadog_profiling_ffi.dll";
pub const PROF_STATIC_LIB_FFI: &str = "datadog_profiling_ffi.lib";
pub const PROF_PDB_FFI: &str = "datadog_profiling_ffi.pdb";
pub const PROF_DLL_IMPORT_LIB_FFI: &str = "datadog_profiling_ffi.dll.lib";
pub const BUILD_CRASHTRACKER: bool = false;
pub const RUSTFLAGS: [&str; 4] = [
    "-C",
    "relocation-model=pic",
    "-C",
    "target-feature=+crt-static",
];

pub fn strip_libraries(_lib_path: &str) {}

pub fn add_additional_files(lib_path: &str, target_path: &OsStr) {
    let from_pdb: PathBuf = [lib_path, PROF_PDB_FFI].iter().collect();
    let to_pdb: PathBuf = [target_path, OsStr::new(PROF_PDB)].iter().collect();
    fs::copy(from_pdb, to_pdb).expect("unable to copy pdb file");

    let from_imp: PathBuf = [lib_path, PROF_DLL_IMPORT_LIB_FFI].iter().collect();
    let to_imp: PathBuf = [target_path, OsStr::new(PROF_DLL_IMPORT_LIB_FFI)]
        .iter()
        .collect();
    fs::copy(from_imp, to_imp).expect("unable to copy dll import lib");
}

pub fn add_pkg_config(_crate_path: &str, _target_path: &str, _version: &str) -> Result<()> {
    Ok(())
}
