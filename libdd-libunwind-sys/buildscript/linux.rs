// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("libunwind_build");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let libunwind_dir = Path::new(&manifest_dir).join("libunwind");

    std::fs::create_dir_all(&build_dir).unwrap();

    assert!(
        libunwind_dir.join("configure").exists(),
        "libunwind/configure is missing."
    );

    // Prevent re-generating the Makefile if already exists
    if !build_dir.join("Makefile").exists() {
        eprintln!("Configuring libunwind...");
        let status = Command::new("sh")
            .current_dir(&build_dir)
            .arg(libunwind_dir.join("configure"))
            .args([
                "--disable-shared",
                "--enable-static",
                "--disable-minidebuginfo",
                "--disable-zlibdebuginfo",
                "--disable-tests",
            ])
            .env("CXXFLAGS", "-fPIC -D_GLIBCXX_USE_CXX11_ABI=0 -O3 -g")
            .env("CFLAGS", "-fPIC -O3 -g")
            .status()
            .expect("Failed to run configure");

        if !status.success() {
            panic!("libunwind configure failed with code: {:?}", status.code());
        }
    }

    eprintln!("Building libunwind...");
    let status = Command::new("sh")
        .current_dir(&build_dir)
        .args(["-c", "make -j$(nproc)"])
        .status()
        .expect("Failed to run make");

    if !status.success() {
        panic!("libunwind make failed with code: {:?}", status.code());
    }

    let lib_file = build_dir.join("src/.libs/libunwind.a");
    if !lib_file.exists() {
        panic!("Failed to locate libunwind.a at: {}", lib_file.display());
    }

    let lib_path = build_dir.join("src/.libs");
    let include_path = build_dir.join("include");

    #[cfg(target_arch = "x86_64")]
    let arch = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let arch = "aarch64";

    println!("cargo:rustc-link-search=native={}", lib_path.display());
    println!("cargo:rustc-link-lib=static=unwind");
    println!("cargo:rustc-link-lib=static=unwind-{}", arch);

    println!("cargo:include={}", include_path.display());
    println!("cargo:lib={}", lib_path.display());
    println!("cargo:libdir={}", lib_path.display());
    println!("cargo:root={}", build_dir.display());

    eprintln!("libunwind library ready at {}", lib_path.display());

    println!("cargo:rerun-if-changed={}/src", libunwind_dir.display());
    println!("cargo:rerun-if-changed={}/include", libunwind_dir.display());
    println!("cargo:rerun-if-changed=build.rs");
}
