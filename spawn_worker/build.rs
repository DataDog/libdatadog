// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use cc_utils::cc;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_os != "windows" {
        println!("cargo:rerun-if-changed=src/fork_shim.c");
        let mut builder = cc::Build::new();
        builder
            .file("src/fork_shim.c")
            .warnings(true)
            .warnings_into_errors(true)
            .emit_rerun_if_env_changed(true);
        if target_os == "macos" {
            const MINIMUM_VERSION: &str = "12.0";
            println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
            match std::env::var("MACOSX_DEPLOYMENT_TARGET") {
                Ok(version) => validate_macos_deployment_target(&version),
                Err(std::env::VarError::NotPresent) => {
                    builder.flag(format!("-mmacosx-version-min={MINIMUM_VERSION}"));
                }
                Err(std::env::VarError::NotUnicode(_)) => {
                    panic!("MACOSX_DEPLOYMENT_TARGET must be valid UTF-8");
                }
            }
        }
        builder.compile("ddog_spawn_worker_fork");
    }

    // Compile the ELF entry point for the shared library (direct exec by ld.so).
    if target_os == "linux" {
        let mut builder = cc::Build::new();
        builder
            .file("src/direct_entry.c")
            .warnings(true)
            .flag("-g")
            .emit_rerun_if_env_changed(true)
            .compile("ddog_spawn_direct_entry");
        // Note, users of direct mode have to add to their build flags:
        // -Wl,-e,ddog_spawn_direct_entry
    }

    let mut builder = cc_utils::ImprovedBuild::new();
    builder
        .file("src/trampoline.c")
        .warnings(true)
        .flag("-g") // DWARF debug info so Valgrind can load symbols before unlink
        .warnings_into_errors(!(target_os == "windows" && target_env == "gnu"))
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
    } else if target_env == "msvc" {
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
        let mut builder = cc_utils::ImprovedBuild::new();
        builder
            .cpp(true)
            .file("src/crashtracking_trampoline.cpp")
            .warnings(true)
            .warnings_into_errors(!(target_os == "windows" && target_env == "gnu"))
            .emit_rerun_if_env_changed(true);

        if target_env == "msvc" {
            builder.flag("/std:c++17").flag("/LD").flag("/EHsc");
        } else {
            builder.flag("-std=c++17");
        }

        builder
            .try_compile_shared_lib("crashtracking_trampoline.bin")
            .unwrap();
    }
}

fn validate_macos_deployment_target(version: &str) {
    let mut components = version.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    let minor = components
        .next()
        .map(|value| value.parse::<u32>().ok())
        .unwrap_or(Some(0));
    let remaining_components_are_valid = components.all(|value| value.parse::<u32>().is_ok());

    let version_tuple = match (major, minor, remaining_components_are_valid) {
        (Some(major), Some(minor), true) => (major, minor),
        _ => panic!(
            "invalid MACOSX_DEPLOYMENT_TARGET {version:?}; expected a dotted numeric version"
        ),
    };
    if version_tuple < (12, 0) {
        panic!("spawn_worker requires MACOSX_DEPLOYMENT_TARGET >= 12.0, got {version}");
    }
}
