// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    let header_name = "profiling.h";
    generate_and_configure_header(header_name);
}
