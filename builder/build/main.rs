// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Rerun build script if any of the env vars change.
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!(
        "cargo:rustc-env=PROFILE={}",
        std::env::var("PROFILE").unwrap()
    );
    println!(
        "cargo:rustc-env=OPT_LEVEL={}",
        std::env::var("OPT_LEVEL").unwrap()
    );
    println!("cargo:rustc-env=DEBUG={}", std::env::var("DEBUG").unwrap());
    println!(
        "cargo:rustc-env=CARGO_PKG_VERSION={}",
        std::env::var("CARGO_PKG_VERSION").unwrap()
    );
    println!("cargo:rustc-env=HOST={}", std::env::var("HOST").unwrap());
    println!(
        "cargo:rustc-env=OUT_DIR={}",
        std::env::var("OUT_DIR").unwrap()
    );
}
