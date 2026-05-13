// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn clang_is_available() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Locate the `gcc-ld/` shim directory shipped with the Rust toolchain.
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
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if target_os != "linux" {
        return;
    }

    println!("cargo:rerun-if-env-changed=LIBDD_OTEL_THREAD_CTX_INLINE");
    println!("cargo:rerun-if-changed=src/tls_shim.c");

    let inline_mode = env::var_os("LIBDD_OTEL_THREAD_CTX_INLINE").is_some();

    let mut build = cc::Build::new();

    if inline_mode {
        assert!(
            clang_is_available(),
            "LIBDD_OTEL_THREAD_CTX_INLINE is set but `clang` was not found. \
             Cross-language LTO requires clang as the C compiler."
        );
        build.compiler("clang");
        build.flag("-flto=thin");

        // Any binary linking this crate in inline mode (including test
        // binaries) needs lld, because -Clinker-plugin-lto passes LTO plugin
        // options that only lld understands.
        if let Some(dir) = find_rust_lld_dir() {
            println!("cargo:rustc-link-arg=-B{}", dir.display());
        }
        println!("cargo:rustc-link-arg=-fuse-ld=lld");
    } else {
        // On x86-64, force TLSDESC via -mtls-dialect=gnu2 (GCC 4.4+, Clang 19+).
        // On aarch64, TLSDESC is the only dynamic TLS model so no flag is needed.
        if target_arch == "x86_64" {
            build.flag("-mtls-dialect=gnu2");
        }
    }

    build.file("src/tls_shim.c").compile("tls_shim");
}
