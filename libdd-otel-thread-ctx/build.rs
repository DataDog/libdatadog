// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use std::env;
use std::process::Command;

fn clang_is_available() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if target_os != "linux" {
        return;
    }

    if !matches!(target_arch.as_str(), "x86_64" | "aarch64") {
        panic!(
            "Unsupported architecture `{}` for otel-thread-ctx on Linux. Only x86_64 and aarch64 are currently supported.",
            target_arch
        )
    }

    println!("cargo:rerun-if-env-changed=LIBDD_OTEL_THREAD_CTX_INLINE");
    println!("cargo:rerun-if-changed=src/tls_shim.c");

    // The otel-thread-ctx FFI crate has a special flag to inline the C shim inside the final
    // library. This setup has additional requirements for the build of this crate, which are
    // enforced below when the flag is set.
    let inline_mode = env::var_os("LIBDD_OTEL_THREAD_CTX_INLINE").is_some_and(|v| v == "1");

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
        if let Some(dir) = build_common::find_rust_lld_dir() {
            println!("cargo:rustc-link-arg=-B{}", dir.display());
        }
        println!("cargo:rustc-link-arg=-fuse-ld=lld");

        // Note: in the inline setup, TLS dialect selection is handled by the linker and is taken
        // care of by the build script of otel-thread-ctx-ffi
    } else if target_arch == "x86_64" {
        // - On aarch64, TLSDESC is already the only dynamic TLS model so no flag is needed.
        // - On x86-64, we use `-mtls-dialect=gnu2` (supported since GCC 4.4 and Clang 19+) to force
        //   the use of TLSDESC as mandated by the spec. If it's not supported, this build will
        //   fail.
        build.flag("-mtls-dialect=gnu2");
    }

    build.file("src/tls_shim.c").compile("tls_shim");
}
