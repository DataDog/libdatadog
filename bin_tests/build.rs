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
        // When definition and reference are in the same PIE executable, the linker
        // relaxes TLSDESC to local-exec and may drop the symbol name from .dynsym.
        // This happens for statically linked languages like Rust where the TLS
        // variable and its consumer are in the same final binary. The same issue
        // applies to production Rust tracers (not just this test binary).
        //
        // We use --dynamic-list to export only the specific symbol (instead of
        // --export-dynamic which exports everything). The assembly anchor provides
        // a weak definition so the symbol survives linking even without TLSDESC.
        let otel_dynlist = out_dir.join("otel_thread_ctx.dynlist");
        std::fs::write(&otel_dynlist, "{ otel_thread_ctx_v1; };\n")
            .expect("failed to write otel dynlist");

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
        println!(
            "cargo:rustc-link-arg=-Wl,--dynamic-list={}",
            otel_dynlist.display()
        );
    }
}

#[cfg(not(unix))]
fn main() {}
