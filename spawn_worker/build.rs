// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use cc_utils::cc;

#[cfg(feature = "trampoline-host-loader")]
#[path = "src/unix/elf_interp.rs"]
mod elf_interp;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    #[cfg(feature = "trampoline-host-loader")]
    if target_os == "linux" {
        let target_pointer_width = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap();
        let target_endian = std::env::var("CARGO_CFG_TARGET_ENDIAN").unwrap();
        assert!(
            target_pointer_width == "64" && target_endian == "little",
            "trampoline-host-loader only supports 64-bit little-endian Linux targets; \
             got {target_pointer_width}-bit {target_endian}-endian"
        );
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

    #[cfg(feature = "trampoline-host-loader")]
    if target_os == "linux" {
        pad_trampoline_interpreter(&mut builder);
    }

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

/// Relinks the trampoline with its default interpreter padded to
/// `elf_interp::INTERP_CAPACITY` bytes, so that the spawner can replace it
/// with the running process's own loader. See `src/unix/elf_interp.rs`.
#[cfg(feature = "trampoline-host-loader")]
fn pad_trampoline_interpreter(builder: &mut cc_utils::ImprovedBuild) {
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("trampoline.bin");
    let elf = std::fs::read(&path).unwrap();
    let Some(interp) = elf_interp::interp_path(&elf) else {
        return; // statically linked: nothing to patch at spawn time
    };
    let padded = elf_interp::padded_interp_path(interp)
        .expect("trampoline interpreter path longer than INTERP_CAPACITY");
    let padded = String::from_utf8(padded).expect("non-UTF-8 trampoline interpreter path");

    builder.flag(&format!("-Wl,--dynamic-linker={padded}"));
    builder.try_compile_executable("trampoline.bin").unwrap();

    let elf = std::fs::read(&path).unwrap();
    assert_eq!(
        elf_interp::interp_range(&elf).map(|r| r.len()),
        Some(elf_interp::INTERP_CAPACITY),
        "linker did not honour the padded --dynamic-linker"
    );
}
