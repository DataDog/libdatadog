// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the shared library built on Linux. Running the sanity check in
//! [libdd_otel_thread_ctx] directly in a Rust test would exercise the static linking case. This
//! test rather checks that the dynamic library is properly linked, which is why it lives within the
//! FFI.
//!
//! Delegates to [`libdd_otel_thread_ctx::autocheck::check_tls_slot_in`] which
//! checks that:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol.
//! - `otel_thread_ctx_v1` follows the TLSDESC access model (if there's a relocation, it's a TLSDESC
//!   one).
//!
//! The cdylib path is derived at runtime from the test executable location.
//! Both the test binary and the cdylib live in `target/<[triple/]profile>/deps/`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn cdylib_path() -> PathBuf {
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .join("liblibdd_otel_thread_ctx_ffi.so")
}

#[test]
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_tls_properties() {
    let path = cdylib_path();
    libdd_otel_thread_ctx::sanity_check::check_tls_slot_in(&path).unwrap();
}
