// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
extern crate build_common;

use build_common::generate_header;
use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let header_name = "telemetry.h";
    let output_base_dir = env::var("DESTDIR").ok(); // Use `ok()` to convert Result to Option

    generate_header(crate_dir, header_name, output_base_dir.as_deref());
    println!("cargo:rerun-if-env-changed=DESTDIR");
}
