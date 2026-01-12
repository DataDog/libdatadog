// Copyright 2025-Present Datadog, Inc.
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
fn main() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let src = PathBuf::from("preload/preload.c");
    let so_path = out_dir.join("libpreload_logger.so");

    let status = Command::new("cc")
        .args(["-fPIC", "-shared", "-Wall", "-Wextra", "-o"])
        .arg(&so_path)
        .arg(&src)
        .status()
        .expect("failed to spawn cc");

    if !status.success() {
        panic!("compiling preload.c failed with status {status}");
    }

    // Make the built shared object path available at compile time for tests/tools.
    println!("cargo:rustc-env=PRELOAD_LOGGER_SO={}", so_path.display());
}

#[cfg(not(unix))]
fn main() {}
