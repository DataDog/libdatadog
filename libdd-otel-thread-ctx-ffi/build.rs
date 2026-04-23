// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    let header_name = "otel-thread-ctx.h";
    generate_and_configure_header(header_name);

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();

    // Export the TLSDESC thread-local variable to the dynamic symbol table so
    // external readers (e.g. the eBPF profiler) can locate it. Rust's cdylib
    // linker applies a version script with `local: *` that hides all symbols
    // not explicitly whitelisted, and also causes lld to relax the TLSDESC
    // access to local-exec (LE), eliminating the dynsym entry entirely.
    // Passing our own version script with an explicit `global:` entry for the
    // symbol beats the `local: *` wildcard and prevents that relaxation.
    //
    // Merging multiple version scripts is not supported by GNU ld, so we also
    // force lld explicitly.
    if target_os == "linux" {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-cdylib-link-arg=-fuse-ld=lld");
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
        );

        // Expose the profile output directory to integration tests so they can
        // locate the cdylib without fragile path-walking.
        // OUT_DIR = <target>/[<triple>/]<profile>/build/<pkg>-<hash>/out
        // Three levels up lands on <target>/[<triple>/]<profile>.
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let profile_dir = std::path::PathBuf::from(&out_dir)
            .ancestors()
            .nth(3)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        println!("cargo:rustc-env=CDYLIB_PROFILE_DIR={profile_dir}");
    }
}
