// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;
use std::env;

fn main() {
    let header_name = "crashtracker.h";
    generate_and_configure_header(header_name);
}
