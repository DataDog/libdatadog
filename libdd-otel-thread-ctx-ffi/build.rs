// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::{find_rust_lld_dir, generate_and_configure_header};
use std::{env, fmt::Display, path::PathBuf, process::Command};

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct LldVersion {
    major: u32,
    minor: u32,
}

impl Display for LldVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Parse the major and minor version from `ld.lld --version` output.
///
/// Typical formats:
///   "LLD 18.1.3 (compatible with GNU linkers)"
///   "LLD 19.1.0"
fn system_lld_version() -> Option<LldVersion> {
    let output = Command::new("ld.lld").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|tok| {
            let mut splitted = tok.split('.');
            let major = splitted.next()?.parse::<u32>().ok()?;
            let minor = splitted.next()?.parse::<u32>().ok()?;

            Some(LldVersion { major, minor })
        })
}

/// TLSDESC is supported in LLD from version 18.1.
const MIN_LLD_VERSION_FOR_TLSDESC: LldVersion = LldVersion {
    major: 18,
    minor: 1,
};

/// Validate that a suitable LLD is available for cross-language LTO.
///
/// Returns the rust-lld `gcc-ld/` directory if found; `None` means the system
/// `ld.lld` will be used instead. Panics with a clear message when the
/// requirements are not met.
fn require_lld_for_inline(target_arch: &str) -> Option<PathBuf> {
    if let Some(dir) = find_rust_lld_dir() {
        return Some(dir);
    }

    match system_lld_version() {
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

    let inline_mode = env::var("LIBDD_OTEL_THREAD_CTX_INLINE").is_ok_and(|v| v == "1");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

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

    // If `LIBDD_OTEL_THREAD_CTX_INLINE` is set to `1`, we try to inline the C shim. See the README
    // for more details.
    if inline_mode {
        let rust_lld_dir = require_lld_for_inline(&target_arch);

        // Emit link args for ALL link types (not just cdylib) so that test binaries also link
        // correctly when RUSTFLAGS sets clang as the linker (in practice we should only build/care
        // about the shared object file in inline mode).
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

    println!(
        "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
    );
}
