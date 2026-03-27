// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
extern crate build_common;

use build_common::generate_and_configure_header;

fn main() {
    let header_name = "library-config.h";
    generate_and_configure_header(header_name);

    // Export the TLSDESC thread-local variable to the dynamic symbol table so
    // external readers (e.g. the eBPF profiler) can locate it.  Rust's cdylib
    // linker applies a version script with `local: *` that hides all symbols
    // not explicitly whitelisted, and also causes lld to relax the TLSDESC
    // access to local-exec (LE), eliminating the dynsym entry entirely.
    // Passing our own version script with an explicit `global:` entry for the
    // symbol beats the `local: *` wildcard and prevents that relaxation.
    //
    // Merging multiple version scripts is not supported by GNU ld, so we also
    // force lld explicitly.
    #[cfg(target_os = "linux")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-fuse-ld=lld");
        println!(
            "cargo:rustc-link-arg=-Wl,--version-script={manifest_dir}/tls-dynamic-list.txt"
        );
    }
}
