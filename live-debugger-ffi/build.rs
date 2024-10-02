// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    let header_name = "live-debugger.h";
    generate_and_configure_header(header_name);
}
