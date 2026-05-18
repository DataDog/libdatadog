// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::path::PathBuf;
use std::process::Command;

#[cfg(not(feature = "cbindgen"))]
pub fn generate_and_configure_header(_header_name: &str) {}
#[cfg(not(feature = "cbindgen"))]
pub fn copy_and_configure_headers() {}

#[cfg(feature = "cbindgen")]
mod cbindgen;
#[cfg(feature = "cbindgen")]
pub use crate::cbindgen::*;

/// Locate the `gcc-ld/` shim directory shipped with the Rust toolchain.
///
/// This directory contains an `ld.lld` wrapper that delegates to `rust-lld`.
/// Passing it via `-B` to the C compiler driver makes it discover rust-lld
/// before any system-wide lld, which
///
/// 1. Avoids the need for a system-wide LLD install.
/// 2. Picks a recent LLD that match the Rust toolchain's LLVM version
pub fn find_rust_lld_dir() -> Option<PathBuf> {
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
