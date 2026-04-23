// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the built cdylib on Linux.
//!
//! These tests check that:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol.
//! - `otel_thread_ctx_v1` is accessed via TLSDESC relocations (R_X86_64_TLSDESC or
//!   R_AARCH64_TLSDESC), as required by the OTel thread-level context sharing spec.
//!
//! The cdylib path is injected at compile time via `build.rs` from `OUT_DIR`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

const SYMBOL: &str = "otel_thread_ctx_v1";

fn cdylib_path() -> PathBuf {
    PathBuf::from(env!("CDYLIB_PROFILE_DIR")).join("liblibdd_otel_thread_ctx_ffi.so")
}

fn readelf(args: &[&str], path: &PathBuf) -> String {
    let out = Command::new("readelf")
        .args(args)
        .arg(path)
        .output()
        .expect("failed to run readelf — is binutils installed?");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn otel_thread_ctx_v1_in_dynsym() {
    let path = cdylib_path();
    let output = readelf(&["-W", "--dyn-syms"], &path);
    let line = output
        .lines()
        .find(|l| l.contains(SYMBOL))
        .unwrap_or_else(|| panic!("'{SYMBOL}' not found in dynsym of {}", path.display()));
    assert!(
        line.contains("TLS") && line.contains("GLOBAL"),
        "'{SYMBOL}' is in dynsym but not as TLS GLOBAL — got:\n  {line}"
    );
}

#[test]
fn otel_thread_ctx_v1_tlsdesc_reloc() {
    let path = cdylib_path();
    let output = readelf(&["-W", "--relocs"], &path);
    let found = output.lines().any(|l| {
        l.contains(SYMBOL)
            && (l.contains("R_X86_64_TLSDESC") || l.contains("R_AARCH64_TLSDESC"))
    });
    assert!(
        found,
        "No TLSDESC relocation found for '{SYMBOL}' in {}\n\
         All relocations mentioning the symbol:\n{}",
        path.display(),
        output
            .lines()
            .filter(|l| l.contains(SYMBOL))
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
