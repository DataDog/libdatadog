// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::{embed_windows_version_info, generate_and_configure_header};

fn main() {
    // Must match libdd-profiling/build.rs: the allocation-failure test reaches the arena
    // observer that libdd-profiling defines only under this cfg.
    println!("cargo:rerun-if-env-changed=LIBDD_ALLOCATION_CHAOS_TEST");
    println!("cargo:rustc-check-cfg=cfg(allocation_chaos_test)");
    if std::env::var_os("LIBDD_ALLOCATION_CHAOS_TEST").is_some_and(|v| v == "1") {
        println!("cargo:rustc-cfg=allocation_chaos_test");
    }

    let header_name = "profiling.h";
    generate_and_configure_header(header_name);
    embed_windows_version_info("datadog_profiling_ffi.dll", "Datadog libdatadog FFI");
}
