// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::{embed_windows_version_info, generate_and_configure_header};

fn main() {
    let header_name = "profiling.h";
    generate_and_configure_header(header_name);
    embed_windows_version_info("datadog_profiling_ffi", "Datadog libdatadog FFI");
}
