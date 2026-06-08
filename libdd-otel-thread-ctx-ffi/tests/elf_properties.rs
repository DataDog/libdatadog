// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the built cdylib on Linux.
//!
//! These tests check that:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol.
//! - `otel_thread_ctx_v1` does NOT use General Dynamic or Local Dynamic TLS relocations
//!   (DTPMOD/DTPOFF). The linker may resolve to TLSDESC or Local Exec; both are acceptable.
//!
//! The cdylib path is derived at runtime from the test executable location.
//! Both the test binary and the cdylib live in `target/<[triple/]profile>/deps/`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

const SYMBOL: &str = "otel_thread_ctx_v1";

fn cdylib_path() -> PathBuf {
    // test binary: target/<[triple/]profile>/deps/<name>
    // cdylib:      target/<[triple/]profile>/deps/liblibdd_otel_thread_ctx_ffi.so
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .join("liblibdd_otel_thread_ctx_ffi.so")
}

fn check_cdylib_readable(path: &PathBuf) {
    assert!(
        std::fs::File::open(path).is_ok(),
        "cdylib at {} could not be opened for reading",
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
    check_cdylib_readable(&path);
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
fn otel_thread_ctx_v1_no_gd_ld_reloc() {
    let path = cdylib_path();
    check_cdylib_readable(&path);
    let output = readelf(&["-W", "--relocs"], &path);

    const FORBIDDEN: &[&str] = &[
        "R_X86_64_DTPMOD64",
        "R_X86_64_DTPOFF64",
        "R_AARCH64_TLS_DTPMOD",
        "R_AARCH64_TLS_DTPREL",
    ];

    let bad_lines: Vec<&str> = output
        .lines()
        .filter(|l| l.contains(SYMBOL) && FORBIDDEN.iter().any(|f| l.contains(f)))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "'{SYMBOL}' has General Dynamic / Local Dynamic relocations in {}:\n{}\n\
         Expected TLSDESC or Local Exec instead.",
        path.display(),
        bad_lines
            .iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
