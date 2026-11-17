// Copyright 2025-Present Datadog, Inc.
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
fn main() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let src = PathBuf::from("preload/preload.c");
    let so_path = out_dir.join("libpreload_logger.so");

    let status = Command::new("cc")
        .args(["-std=gnu11", "-fPIC", "-shared", "-Wall", "-Wextra", "-o"])
        .arg(&so_path)
        .arg(&src)
        .status()
        .expect("failed to spawn cc");

    if !status.success() {
        panic!("compiling preload.c failed with status {status}");
    }

    // Make the built shared object path available at compile time for tests/tools.
    println!("cargo:rustc-env=PRELOAD_LOGGER_SO={}", so_path.display());

    // --- trigger_assert static library (Linux only) ---
    #[cfg(target_os = "linux")]
    {
        let assert_src = PathBuf::from("src/c/trigger_assert.c");
        let assert_obj = out_dir.join("trigger_assert.o");
        let assert_lib = out_dir.join("libtrigger_assert.a");

        let status = Command::new("cc")
            .args([
                "-std=c11", "-fPIC", "-UNDEBUG", "-Wall", "-Wextra", "-c", "-o",
            ])
            .arg(&assert_obj)
            .arg(&assert_src)
            .status()
            .expect("failed to compile trigger_assert.c");
        if !status.success() {
            panic!("compiling trigger_assert.c failed with status {status}");
        }

        let status = Command::new("ar")
            .args(["rcs"])
            .arg(&assert_lib)
            .arg(&assert_obj)
            .status()
            .expect("failed to run ar");
        if !status.success() {
            panic!("creating libtrigger_assert.a failed with status {status}");
        }

        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=trigger_assert");
        println!("cargo:rerun-if-changed=src/c/trigger_assert.c");

        // Export otel_thread_ctx_v1 to the dynamic symbol table (.dynsym).
        //
        // The OTel thread context spec requires the symbol in .dynsym even for
        // executables. However, when definition and reference are in the same PIE
        // executable, both lld and GNU ld relax TLSDESC accesses to TP-relative,
        // eliminating the symbol name from .dynsym (the TLS allocation survives but
        // the name doesn't). This only affects this test binary — in production the
        // publisher is always in a separate .so where names are preserved.
        //
        // Fix: assemble a weak definition of the symbol that the linker merges with
        // the strong one from libdd-otel-thread-ctx's global_asm!. The weak definition
        // isn't subject to relaxation-based name elimination, so the name survives for
        // --export-dynamic to promote into .dynsym. --export-dynamic is universally
        // supported (GNU ld >= 2.17, lld), unlike --export-dynamic-symbol (>= 2.35).
        let otel_asm_src = out_dir.join("otel_tls_anchor.s");
        let otel_asm_obj = out_dir.join("otel_tls_anchor.o");
        std::fs::write(
            &otel_asm_src,
            ".section .tbss,\"awT\",@nobits\n\
             .weak otel_thread_ctx_v1\n\
             .type  otel_thread_ctx_v1, @tls_object\n\
             .size  otel_thread_ctx_v1, 8\n\
             .balign 8\n\
             otel_thread_ctx_v1:\n\
             .zero  8\n\
             .previous\n",
        )
        .expect("failed to write otel TLS anchor assembly");

        let status = Command::new("cc")
            .args(["-c", "-o"])
            .arg(&otel_asm_obj)
            .arg(&otel_asm_src)
            .status()
            .expect("failed to assemble otel_tls_anchor.s");
        if !status.success() {
            panic!("assembling otel_tls_anchor.s failed with status {status}");
        }

        println!("cargo:rustc-link-arg={}", otel_asm_obj.display());
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
    }
}

#[cfg(not(unix))]
fn main() {}
