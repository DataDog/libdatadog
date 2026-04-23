// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the built cdylib on Linux.
//!
//! These tests check that:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol.
//! - `otel_thread_ctx_v1` is accessed via TLSDESC relocations (R_X86_64_TLSDESC or
//!   R_AARCH64_TLSDESC), as required by the OTel thread-level context sharing spec.
//!
//! The cdylib path is derived at runtime from the test executable location:
//! the test binary lives in `target/<[triple/]profile>/deps/`, so the cdylib
//! is one level up at `target/<[triple/]profile>/liblibdd_otel_thread_ctx_ffi.so`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

const SYMBOL: &str = "otel_thread_ctx_v1";

fn cdylib_path() -> PathBuf {
    // test binary: target/<[triple/]profile>/deps/<name>
    // cdylib:      target/<[triple/]profile>/liblibdd_otel_thread_ctx_ffi.so
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent() // deps/
        .and_then(|p| p.parent()) // <profile>/
        .expect("unexpected test executable path structure")
        .join("liblibdd_otel_thread_ctx_ffi.so")
}

fn check_cdylib_exists(path: &PathBuf) {
    assert!(
        path.exists(),
        "cdylib not found at {}: \
         ensure `cargo build -p libdd-otel-thread-ctx-ffi` has been run for this profile",
        path.display()
    );
    assert!(
        std::fs::File::open(path).is_ok(),
        "cdylib exists at {} but could not be opened for reading",
        path.display()
    );
}

fn readelf(args: &[&str], path: &PathBuf) -> String {
    let out = Command::new("readelf")
        .args(args)
        .arg(path)
        .output()
        .expect("failed to run readelf. Is binutils installed?");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_in_dynsym() {
    let path = cdylib_path();
    check_cdylib_exists(&path);
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
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_tlsdesc_reloc() {
    let path = cdylib_path();
    check_cdylib_exists(&path);
    let output = readelf(&["-W", "--relocs"], &path);
    let found = output.lines().any(|l| {
        l.contains(SYMBOL) && (l.contains("R_X86_64_TLSDESC") || l.contains("R_AARCH64_TLSDESC"))
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
