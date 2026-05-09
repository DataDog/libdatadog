// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use cc_utils::cc;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Compile the ELF entry point for the shared library (direct exec by ld.so).
    if target_os == "linux" {
        // Detect whether execinfo (backtrace()) is available.
        // On glibc it's in libc itself; on musl a separate -lexecinfo may be needed.
        // Use a fresh build to probe; don't reuse the main build that has direct_entry.c.
        let probe_result = cc::Build::new()
            .file("src/check_execinfo.c")
            .try_compile("check_execinfo_probe");
        let have_execinfo = probe_result.is_ok();
        println!("cargo:warning=execinfo probe: {} ({:?})", have_execinfo, probe_result.err());

        let mut build = cc::Build::new();
        build.file("src/direct_entry.c");
        if have_execinfo {
            build.define("HAVE_BACKTRACE", "1");
            // On musl, backtrace() lives in libexecinfo (separate package).
            // On glibc, it is already in libc — adding -lexecinfo would fail.
            let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
            if target_env == "musl" {
                println!("cargo:rustc-link-lib=execinfo");
            }
        }

        build.compile("ddtrace_direct_entry");
        println!("cargo:rerun-if-changed=src/direct_entry.c");
        println!("cargo:rerun-if-changed=src/check_execinfo.c");
        // Note, users of direct mode have to add to their build flags:
        // -Wl,-e,ddog_sidecar_direct_entry
    }

    let mut builder = cc_utils::ImprovedBuild::new();
    builder
        .file("src/trampoline.c")
        .warnings(true)
        .flag("-g") // DWARF debug info so Valgrind can load symbols before unlink
        .link_dynamically("dl")
        .warnings_into_errors(true)
        .emit_rerun_if_env_changed(true);

    if target_os != "windows" {
        builder.link_dynamically("dl");
        if cfg!(target_os = "linux") {
            builder.flag("-Wl,--no-as-needed");
        }
        // rust code generally requires libm. Just link against it.
        builder.link_dynamically("m");
        // some old libc versions are unhappy if it gets linked in dynamically later on
        builder.link_dynamically("pthread");
    } else {
        builder.flag("-wd4996"); // disable deprecation warnings
    }

    builder.try_compile_executable("trampoline.bin").unwrap();

    if target_os != "windows" {
        cc_utils::ImprovedBuild::new()
            .file("src/ld_preload_trampoline.c")
            .link_dynamically("dl")
            .warnings(true)
            .warnings_into_errors(true)
            .emit_rerun_if_env_changed(true)
            .try_compile_shared_lib("ld_preload_trampoline.shared_lib")
            .unwrap();
    } else {
        cc_utils::ImprovedBuild::new()
            .file("src/crashtracking_trampoline.cpp") // Path to your C++ file
            .warnings(true)
            .warnings_into_errors(true)
            .flag("/std:c++17") // Set the C++ standard (adjust as needed)
            .flag("/LD")
            .flag("/EHsc")
            .emit_rerun_if_env_changed(true)
            .try_compile_shared_lib("crashtracking_trampoline.bin")
            .unwrap();
    }
}
