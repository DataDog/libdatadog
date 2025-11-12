// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    println!("cargo:rerun-if-changed=src/*");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=build.rs");
    let header_name = "ffe.h";
    generate_and_configure_header(header_name);
}
