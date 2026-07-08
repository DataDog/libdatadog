// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the shared library built on Linux. Running the sanity check in
//! [libdd_otel_thread_ctx] directly in a Rust test would exercise the static linking case. This
//! test rather checks that the dynamic library is properly linked, which is why it lives within the
//! FFI.
//!
//! This checks that, in the linked `cdylib`:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol;
//! - it follows the TLSDESC access model: if there is a relocation for it, it is a TLSDESC
//!   relocation.
//!
//! The complementary check that our inline assembly emits the exact TLSDESC instruction sequence a
//! compiler would (so the linker relaxes it correctly) lives in `tlsdesc_inline_sequence.rs`.
//!
//! Library artifact paths are derived at runtime from the test executable location.

#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use libdd_otel_thread_ctx::test_utils::artifacts::{artifact_path, check_readable};

#[test]
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_tls_properties() {
    let path = artifact_path("liblibdd_otel_thread_ctx_ffi.so");
    check_readable(&path);
    libdd_otel_thread_ctx::sanity_check::check_tls_slot_in(&path).unwrap();
}
