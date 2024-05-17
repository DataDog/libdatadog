// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;

pub use cc_utils::cc;

fn main() {
    let mut builder = cc_utils::ImprovedBuild::new();
    builder
        .file("src/trampoline.c")
        .warnings(true)
        .link_dynamically("dl")
        .warnings_into_errors(true)
        .flag("-std=c99")
        .emit_rerun_if_env_changed(true);

    if !env::var("TARGET").unwrap().contains("windows") {
        builder.link_dynamically("dl");
        if cfg!(target_os = "linux") {
            builder.flag("-Wl,--no-as-needed");
        }
        builder.link_dynamically("m"); // rust code generally requires libm. Just link against it.
    } else {
        builder.flag("-wd4996"); // disable deprecation warnings
    }

    builder.try_compile_executable("trampoline.bin").unwrap();

    if !env::var("TARGET").unwrap().contains("windows")  {
        cc_utils::ImprovedBuild::new()
            .file("src/ld_preload_trampoline.c")
            .link_dynamically("dl")
            .warnings(true)
            .warnings_into_errors(true)
            .emit_rerun_if_env_changed(true)
            .try_compile_shared_lib("ld_preload_trampoline.shared_lib")
            .unwrap();
    }
}
