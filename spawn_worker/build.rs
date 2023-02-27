// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub use cc_utils::cc;

fn main() {
    cc_utils::ImprovedBuild::new()
        .file("src/trampoline.c")
        .warnings(true)
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true)
        .try_compile_executable("trampoline.bin")
        .unwrap();

    if !cfg!(target_os = "windows") {
        cc_utils::ImprovedBuild::new()
            .file("src/ld_preload_trampoline.c")
            .warnings(true)
            .warnings_into_errors(true)
            .emit_rerun_if_env_changed(true)
            .try_compile_shared_lib("ld_preload_trampoline.shared_lib")
            .unwrap();
    }
}
