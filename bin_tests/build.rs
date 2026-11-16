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
        .args(["-std=gnu11", "-fPIC", "-shared", "-Wall", "-Wextra", "-o"])
        .arg(&so_path)
        .arg(&src)
        .status()
        .expect("failed to spawn cc");

    if !status.success() {
        panic!("compiling preload.c failed with status {status}");
    }

    // Make the built shared object path available at compile time for tests/tools.
    println!("cargo:rustc-env=PRELOAD_LOGGER_SO={}", so_path.display());

    // --- trigger_assert static library (Linux only) ---
    #[cfg(target_os = "linux")]
    {
        let assert_src = PathBuf::from("src/c/trigger_assert.c");
        let assert_obj = out_dir.join("trigger_assert.o");
        let assert_lib = out_dir.join("libtrigger_assert.a");

        let status = Command::new("cc")
            .args([
                "-std=c11", "-fPIC", "-UNDEBUG", "-Wall", "-Wextra", "-c", "-o",
            ])
            .arg(&assert_obj)
            .arg(&assert_src)
            .status()
            .expect("failed to compile trigger_assert.c");
        if !status.success() {
            panic!("compiling trigger_assert.c failed with status {status}");
        }

        let status = Command::new("ar")
            .args(["rcs"])
            .arg(&assert_lib)
            .arg(&assert_obj)
            .status()
            .expect("failed to run ar");
        if !status.success() {
            panic!("creating libtrigger_assert.a failed with status {status}");
        }

        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=trigger_assert");
        println!("cargo:rerun-if-changed=src/c/trigger_assert.c");

        // Export otel_thread_ctx_v1 to the dynamic symbol table. The OTel thread
        // context spec requires this even for executables (not just shared libraries).
        // https://github.com/open-telemetry/opentelemetry-specification/blob/a4e05d9704fc5218d85a4154fb5b143ea3f5340e/oteps/profiles/4947-thread-ctx.md?plain=1#L93
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic-symbol=otel_thread_ctx_v1");
    }
}

#[cfg(not(unix))]
fn main() {}
