// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;
use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Locate the `gcc-ld/` shim directory shipped with the Rust toolchain.
///
/// This directory contains an `ld.lld` wrapper that delegates to `rust-lld`.
/// Passing it via `-B` to the C compiler driver makes it discover rust-lld
/// before any system-wide lld, which matters when the system lld is too old
/// (e.g. lld 7 on CentOS 7 cannot handle TLSDESC relocations).
///
/// The discovery goes through `rustc --print sysroot` so it works on any
/// layout (NixOS, Homebrew, rustup, distro packages) without assuming FHS.
fn find_rust_lld_dir() -> Option<PathBuf> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let target = env::var("TARGET").ok()?;

    let output = Command::new(&rustc)
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;

    let sysroot = std::str::from_utf8(&output.stdout).ok()?.trim();
    let dir = PathBuf::from(sysroot)
        .join("lib/rustlib")
        .join(&target)
        .join("bin/gcc-ld");

    dir.join("ld.lld").exists().then_some(dir)
}

fn main() {
    generate_and_configure_header("otel-thread-ctx.h");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    // Export the TLSDESC thread-local variable to the dynamic symbol table so
    // external readers (e.g. the eBPF profiler) can discover it. Rust's cdylib
    // linker applies a version script with `local: *` that hides all symbols
    // not explicitly allowlisted, and also causes lld to relax the TLSDESC
    // access to local-exec (LE), eliminating the dynsym entry entirely.
    // Passing our own version script with an explicit `global:` entry for the
    // symbol beats the `local: *` wildcard and prevents that relaxation.
    //
    // Merging multiple version scripts is not supported by GNU ld, so we need
    // lld. We prefer the toolchain's bundled rust-lld (LLD 19+ since Rust 1.84)
    // over the system lld, because some environments ship an lld too old for
    // TLSDESC (e.g. lld 7 on CentOS 7). If rust-lld is not found we fall back
    // to whatever `lld` the system provides.
    if target_os == "linux" {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

        if let Some(gcc_ld_dir) = find_rust_lld_dir() {
            println!("cargo:rustc-cdylib-link-arg=-B{}", gcc_ld_dir.display());
        }
        println!("cargo:rustc-cdylib-link-arg=-fuse-ld=lld");
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
        );
    }
}
