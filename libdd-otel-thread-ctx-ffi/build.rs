// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;
use std::env;

fn main() {
    generate_and_configure_header("otel-thread-ctx.h");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    // Export the TLSDESC thread-local variable to the dynamic symbol table so
    // external readers (e.g. the eBPF profiler) can locate it. Rust's cdylib
    // linker applies a version script with `local: *` that hides all symbols
    // not explicitly whitelisted, and also causes lld to relax the TLSDESC
    // access to local-exec (LE), eliminating the dynsym entry entirely.
    // Passing our own version script with an explicit `global:` entry for the
    // symbol beats the `local: *` wildcard and prevents that relaxation.
    //
    // Merging multiple version scripts is not supported by GNU ld, but lld (or rather rust-lld) is
    // the default linker picked by Rust since v1.70+. We use to force lld via:
    // `println!("cargo:rustc-cdylib-link-arg=-fuse-ld=lld")` but this caused extra pain in CI
    // because it requires a system wide lld install (instead of using rust-lld). Since we have a
    // test to check that the symbol was properly propagated, we rather let Rust pick lld which is
    // preferred anyway, and if it's not possible in an exotic build setup, the test should catch
    // it.
    if target_os == "linux" {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
        );
    }
}
