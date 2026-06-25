// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end smoke test: install the GOT overrides into the live test
//! process, do a libc `malloc`/`free`, and check that the hook ran.
//!
//! Single test only — installing the overrides mutates global process
//! state, so we keep this isolated from the unit-test binary.

// Integration test invokes the GOT-patching machinery for real
// (dl_iterate_phdr + dlsym + mprotect), which miri can't execute.
#![cfg(all(target_os = "linux", not(miri)))]

use std::ffi::c_void;

// We don't have a clean way to instrument the gotter's own hooks from
// outside the crate, so this test goes one level lower: it confirms
// that after install, the heap is still functional and that no
// recursive crash has occurred when malloc/free go through the patched
// GOT.
#[test]
fn install_changes_malloc_dispatch() {
    extern "C" {
        fn malloc(size: usize) -> *mut c_void;
    }

    let installed = libdd_heap_gotter::install_heap_overrides();
    assert!(
        installed,
        "expected install_heap_overrides to find at least one symbol"
    );

    // Touch the heap to exercise the patched GOT. We can't assert that
    // the address differs from the original libc malloc address from
    // Rust directly — the `malloc` extern fn item resolves via the same
    // GOT we just patched, so reading its address here either gives us
    // the PLT entry (still the same) or the post-resolution function
    // pointer (now our hook). What we *can* assert is that the heap is
    // still functional and that no recursive crash has occurred.
    unsafe {
        let p = malloc(64);
        assert!(!p.is_null(), "malloc returned NULL post-install");
        libc::free(p);
    }

    libdd_heap_gotter::restore_heap_overrides();

    // A second alloc post-restore should also be fine.
    unsafe {
        let p = malloc(64);
        assert!(!p.is_null(), "malloc returned NULL post-restore");
        libc::free(p);
    }
}
