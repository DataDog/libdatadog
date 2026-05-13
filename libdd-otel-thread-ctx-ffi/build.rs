// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;
use std::{env, path::PathBuf, process::Command};

/// Locate the `gcc-ld/` shim directory shipped with the Rust toolchain.
///
/// This directory contains an `ld.lld` wrapper that delegates to `rust-lld`.
/// Passing it via `-B` to the C compiler driver makes it discover rust-lld
/// before any system-wide lld, which
///
/// 1. Avoids the need for a system-wide LLD install.
/// 2. Picks a recent LLD, as opposed to e.g. CentOS 7's LLVM 7 which is too
///    old to handle TLSDESC relocations properly.
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

/// Parse the major version from `ld.lld --version` output.
///
/// Typical formats:
///   "LLD 18.1.3 (compatible with GNU linkers)"
///   "LLD 19.1.0"
fn system_lld_major_version() -> Option<u32> {
    let output = Command::new("ld.lld").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find_map(|tok| tok.split('.').next()?.parse::<u32>().ok())
}

const MIN_LLD_VERSION_FOR_TLSDESC: u32 = 18;

/// Validate that a suitable LLD is available for cross-language LTO.
///
/// Returns the rust-lld `gcc-ld/` directory if found; `None` means the system
/// `ld.lld` will be used instead. Panics with a clear message when the
/// requirements are not met.
fn require_lld_for_inline(target_arch: &str) -> Option<PathBuf> {
    if let Some(dir) = find_rust_lld_dir() {
        return Some(dir);
    }

    match system_lld_major_version() {
        Some(v) if target_arch != "x86_64" || v >= MIN_LLD_VERSION_FOR_TLSDESC => None,
        Some(v) => panic!(
            "LIBDD_OTEL_THREAD_CTX_INLINE requires LLD >= {MIN_LLD_VERSION_FOR_TLSDESC} on \
             x86-64 (for -mllvm -enable-tlsdesc), but system ld.lld is version {v}. \
             Install a newer LLD or use a Rust toolchain that bundles rust-lld."
        ),
        None => panic!(
            "LIBDD_OTEL_THREAD_CTX_INLINE requires LLD for cross-language LTO, but neither \
             rust-lld nor a system ld.lld was found."
        ),
    }
}

fn main() {
    generate_and_configure_header("otel-thread-ctx.h");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "linux" {
        return;
    }

    println!("cargo:rerun-if-env-changed=LIBDD_OTEL_THREAD_CTX_INLINE");

    let inline_mode = env::var("LIBDD_OTEL_THREAD_CTX_INLINE").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if &inline_mode == "1" {
        let rust_lld_dir = require_lld_for_inline(&target_arch);

        // Emit link args for ALL link types (not just cdylib) so that test
        // binaries also link correctly when RUSTFLAGS sets clang as the linker (although we should
        // only build the shared object file in inline mode).
        if let Some(dir) = rust_lld_dir {
            println!("cargo:rustc-link-arg=-B{}", dir.display());
        }
        println!("cargo:rustc-link-arg=-fuse-ld=lld");

        // On x86-64, tell the LLVM backend to use TLSDESC during LTO codegen.
        // On aarch64 TLSDESC is the default and the only model.
        if target_arch == "x86_64" {
            println!("cargo:rustc-link-arg=-Wl,-mllvm,-enable-tlsdesc");
        }
    } else {
        // Default mode: only the cdylib needs lld (for the version script).
        if let Some(gcc_ld_dir) = find_rust_lld_dir() {
            println!("cargo:rustc-cdylib-link-arg=-B{}", gcc_ld_dir.display());
        }
        println!("cargo:rustc-cdylib-link-arg=-fuse-ld=lld");
    }

    // Version script exports the TLS symbol to the dynamic symbol table so
    // external readers (eBPF profiler) can discover it.
    println!(
        "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
    );
}
