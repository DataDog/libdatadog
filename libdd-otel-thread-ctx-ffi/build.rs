// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::{find_rust_lld_dir, generate_and_configure_header};
use std::env;

fn main() {
    generate_and_configure_header("otel-thread-ctx.h");

    let cross_compiling = env::var("HOST").unwrap() != env::var("TARGET").unwrap();
    println!("cargo:rustc-env=LIBDD_OTEL_THREAD_CTX_FFI_CROSS_COMPILING={cross_compiling}");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "linux" {
        return;
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    // Export the TLSDESC thread-local variable to the dynamic symbol table so external readers
    // (e.g. the eBPF profiler) can discover it. Rust's cdylib linker applies a version script with
    // `local: *` that hides all symbols not explicitly allowlisted, and also causes lld to relax
    // the TLSDESC access, eliminating the dynsym entry entirely.
    //
    // Passing our own version script with an explicit `global:` entry for the symbol beats the
    // `local: *` wildcard and prevents that relaxation.
    //
    // Merging multiple version scripts is not supported by GNU ld, so we need lld. We prefer the
    // toolchain's bundled rust-lld (LLD 19+ since Rust 1.84) over the system lld (if it even
    // exists). If rust-lld is not found we fall back to whatever `lld` the system provides.
    if let Some(gcc_ld_dir) = find_rust_lld_dir() {
        println!("cargo:rustc-cdylib-link-arg=-B{}", gcc_ld_dir.display());
    }
    println!("cargo:rustc-cdylib-link-arg=-fuse-ld=lld");

    println!(
        "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
    );
}
