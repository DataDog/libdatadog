// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    println!("cargo:rerun-if-changed=src");
    let header_name = "profiling.h";
    generate_and_configure_header(header_name);
}
