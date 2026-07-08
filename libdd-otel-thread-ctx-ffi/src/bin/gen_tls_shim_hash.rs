// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generate the "golden" hash of the TLSDESC access sequence for `otel_thread_ctx_v1` from a
//! clang's output for a small C shim (inlined as [`TLS_SHIM_C`], written to a temp file and
//! compiled at runtime).
//!
//! This is the reference side of the `tlsdesc_inline_sequence` integration test: the test hashes
//! the sequence our inline assembly produces and asserts it equals the hash printed here. When the
//! inline assembly (or the pinned toolchain) legitimately changes, re-run this to obtain the new
//! hash and paste it into the test.
//!
//! This is a dev tool, not part of a normal build: it lives behind the `gen-tls-shim-hash` feature
//! (via the `[[bin]]` `required-features`), so it is only compiled when that feature is enabled.
//!
//! # Reference compilers
//!
//! - **aarch64** uses `clang` (cross-compiling with `--target=`): TLSDESC is the default dialect
//!   for a `global-dynamic` access, so no extra flag is needed.
//! - **x86-64** uses `gcc` with `-mtls-dialect=gnu2`. The pinned CI image below ships Clang 18,
//!   which predates x86-64 TLSDESC support (`-mtls-dialect=gnu2` is Clang 19+), so we fall back to
//!   gcc there, which is just a work-around. The clang and gcc sequences are currently byte-by-byte
//!   identical for x86_64 anyway. Feel free to update to using clang once Clang19+ lands in CI
//!   images.
//!  
//! The compilers can be overridden with `$CC` (x86-64) and `$CLANG` (aarch64).
//!
//! # Reproducibility
//!
//! For a *reproducible* hash, run this inside the latest [Ubuntu Datadog CI build
//! image](https://github.com/DataDog/libddprof-build/blob/main/ubuntu.Dockerfile). The current hash
//! was built from rev `758a114281f3545b40598272ea1c41404b43f2f6`.
//!
//! ```text
//! docker run --rm -v "$PWD":/repo:ro -e CARGO_TARGET_DIR=/tmp/target -e CARGO_HOME=/tmp/cargo \
//!   -w /repo registry.ddbuild.io/ci/libddprof-build:dependencies_ubuntu_30 \
//!   bash -c 'for a in x86_64 aarch64; do \
//!     cargo run --quiet --no-default-features --features gen-tls-shim-hash \
//!       --bin gen_tls_shim_hash -p libdd-otel-thread-ctx-ffi -- "$a"; done'
//! ```

use std::{path::Path, process::Command};

use libdd_otel_thread_ctx::test_utils::tls_shim_window::{windows_in_object_file, Arch};

/// The reference C translation unit: a public `otel_thread_ctx_v1` TLS symbol compiled as a shared
/// object plus an `accessor, so the compiler emits exactly one TLSDESC access sequence. It is
/// written to a temp file and compiled at runtime.
const TLS_SHIM_C: &str = r#"extern __thread void *otel_thread_ctx_v1 __attribute__((tls_model("global-dynamic")));

__attribute__((noinline)) void **tls_slot_from_c(void) {
    return &otel_thread_ctx_v1;
}
"#;

/// Compile `tls_shim.c` for `arch` with the flags that produce a `global-dynamic` TLSDESC access,
/// matching the access model our inline assembly implements. See the module docs for why x86-64
/// uses gcc and aarch64 uses Clang.
fn compile_reference(arch: Arch, source: &Path, object: &Path) {
    let mut cmd = match arch {
        Arch::X86_64 => {
            // gcc: the psABI-canonical x86-64 TLSDESC sequence. `gnu2` selects the TLSDESC dialect.
            let cc = std::env::var("CC").unwrap_or_else(|_| "gcc".to_owned());
            let mut cmd = Command::new(cc);
            cmd.arg("-mtls-dialect=gnu2");
            cmd
        }
        Arch::Aarch64 => {
            // clang, cross-compiling: TLSDESC is the default dialect for a `global-dynamic` access.
            let clang = std::env::var("CLANG").unwrap_or_else(|_| "clang".to_owned());
            let mut cmd = Command::new(clang);
            cmd.arg(format!("--target={}", arch.clang_target_triple()));
            cmd
        }
    };
    cmd.args(["-O2", "-fPIC", "-fomit-frame-pointer", "-c"]);
    cmd.arg(source).arg("-o").arg(object);

    eprintln!("running: {cmd:?}");
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to run reference compiler: {e}"));
    assert!(
        status.success(),
        "reference compiler failed with status {status}"
    );
}

fn main() {
    let arch = match std::env::args().nth(1) {
        Some(arg) => arg.parse().unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(2);
        }),
        None => Arch::host(),
    };

    let out_dir = std::env::temp_dir().join(format!("gen_tls_shim_hash-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", out_dir.display()));
    let source = out_dir.join("tls_shim.c");
    std::fs::write(&source, TLS_SHIM_C)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", source.display()));
    let object = out_dir.join(format!("tls_shim_{arch:?}.o"));

    compile_reference(arch, &source, &object);

    let windows = windows_in_object_file(&object);
    assert_eq!(
        windows.len(),
        1,
        "expected exactly one TLSDESC access in the reference object {}; found {}",
        object.display(),
        windows.len()
    );

    let window = &windows[0];
    assert_eq!(
        window.arch, arch,
        "reference object architecture ({:?}) does not match requested architecture ({arch:?})",
        window.arch
    );

    let _ = std::fs::remove_dir_all(&out_dir);

    println!("arch:  {arch:?}");
    println!("bytes: {}", window.hex_dump());
    println!("hash:  {}", window.hash_hex());
}
