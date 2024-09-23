// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub const NATIVE_LIBS: &str = "";
pub const PROF_DYNAMIC_LIB: &str = "datadog_profiling.dll";
pub const PROF_STATIC_LIB: &str = "datadog_profiling.lib";
pub const PROF_DYNAMIC_LIB_FFI: &str = "datadog_profiling_ffi.dll";
pub const PROF_STATIC_LIB_FFI: &str = "datadog_profiling_ffi.lib";
pub const REMOVE_RPATH: bool = false;
pub const BUILD_CRASHTRACKER: bool = false;

pub fn fix_rpath(_lib_path: &str) {}
pub fn strip_libraries(_lib_path: &str) {}
pub fn fix_soname(_lib_path: &str) {}
