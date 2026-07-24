// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Build script for `libdd-profiling-heap-gotter-ffi`. Generates the C header for
//! the FFI surface via cbindgen.

extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    println!("cargo:rerun-if-changed=src/*");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=build.rs");
    generate_and_configure_header("heap_gotter.h");
}
