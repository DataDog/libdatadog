// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end smoke tests: install the GOT overrides into the live test
//! process, exercise libc allocator functions, and confirm the sampler
//! is actually intercepting them.
//!
//! These tests mutate global process state (GOT entries), so they must
//! not run in parallel with each other. The `#[serial]` attribute
//! enforces that.

// Integration tests invoke GOT-patching machinery for real
// (dl_iterate_phdr + mprotect), which miri can't execute.
#![cfg(all(target_os = "linux", target_pointer_width = "64", not(miri)))]

use std::ffi::c_void;

use libdd_heap_sampler::{dd_sample_flag_check_fast, dd_tl_state_get, dd_tl_state_get_or_init};
use serial_test::serial;

/// After install the heap should still be functional and no recursive
/// crash should occur when malloc/free go through the patched GOT.
#[test]
#[serial]
fn install_and_restore_keeps_heap_functional() {
    extern "C" {
        fn malloc(size: usize) -> *mut c_void;
    }

    let installed = libdd_heap_gotter::install_heap_overrides();
    assert!(
        installed,
        "expected install_heap_overrides to find at least one symbol"
    );

    unsafe {
        let p = malloc(64);
        assert!(!p.is_null(), "malloc returned NULL post-install");
        libc::free(p);
    }

    libdd_heap_gotter::restore_heap_overrides();

    unsafe {
        let p = malloc(64);
        assert!(!p.is_null(), "malloc returned NULL post-restore");
        libc::free(p);
    }
}

/// Confirm that after install, allocations actually flow through the
/// sampler and produce tagged pointers. We force sampling_interval=1
/// so every allocation is sampled, then check that libc::malloc returns
/// a pointer carrying the sample flag.
#[test]
#[serial]
fn install_produces_sampled_allocations() {
    let installed = libdd_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        // Ensure sampler TLS is materialised on this thread.
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());

        // Force every allocation to be sampled.
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;

        // Allocate. The gotter hook should intercept this, the sampler
        // should decide to sample (interval=1), and the returned
        // pointer should carry the sample flag.
        let p = libc::malloc(128);
        assert!(!p.is_null());

        let mut raw: *mut c_void = std::ptr::null_mut();
        let sampled = dd_sample_flag_check_fast(p, &mut raw);
        assert!(
            sampled,
            "expected malloc to return a sampled pointer with interval=1"
        );
        assert!(!raw.is_null());

        // Free via libc::free which goes through gotter_free; it
        // should handle the tagged pointer correctly.
        libc::free(p);

        // Restore the default interval so we don't mess with anything
        // after the test.
        (*tl).sampling_interval = 512 * 1024;
    }

    libdd_heap_gotter::restore_heap_overrides();
}

/// Same as above but for realloc: confirm a sampled allocation that
/// gets reallocated comes back as a valid (unsampled) pointer with the
/// user data intact.
#[test]
#[serial]
fn realloc_of_sampled_allocation_preserves_data() {
    let installed = libdd_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;

        // Allocate and write a pattern.
        let p = libc::malloc(64) as *mut u8;
        assert!(!p.is_null());
        for i in 0..64u8 {
            *p.add(i as usize) = i;
        }

        // check_fast destructively clears the flag, so we can't peek
        // and then realloc the same pointer. Just free this one and
        // allocate a fresh one for the realloc test.
        libc::free(p as *mut c_void);

        // Fresh sampled allocation for the realloc test.
        let p = libc::malloc(64) as *mut u8;
        assert!(!p.is_null());
        for i in 0..64u8 {
            *p.add(i as usize) = 0xAB ^ i;
        }

        // Realloc to a larger size. After realloc the pointer should be
        // valid (possibly unsampled per the MVP model) and the original
        // data should be preserved.
        let p2 = libc::realloc(p as *mut c_void, 256) as *mut u8;
        assert!(!p2.is_null(), "realloc returned NULL");

        // Verify data integrity.
        for i in 0..64u8 {
            let got = *p2.add(i as usize);
            assert_eq!(
                got,
                0xAB ^ i,
                "data corruption at byte {i}: expected 0x{:02x}, got 0x{got:02x}",
                0xAB ^ i
            );
        }

        libc::free(p2 as *mut c_void);
        (*tl).sampling_interval = 512 * 1024;
    }

    libdd_heap_gotter::restore_heap_overrides();
}

/// Confirm that after restore, allocations are no longer sampled.
#[test]
#[serial]
fn restore_stops_sampling() {
    let installed = libdd_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;
    }

    libdd_heap_gotter::restore_heap_overrides();

    // After restore, malloc should return a plain pointer with no
    // sample flag, even though the sampler TLS still has interval=1.
    unsafe {
        let p = libc::malloc(128);
        assert!(!p.is_null());

        let mut raw: *mut c_void = std::ptr::null_mut();
        let sampled = dd_sample_flag_check_fast(p, &mut raw);
        assert!(
            !sampled,
            "expected malloc to return an unsampled pointer after restore"
        );

        libc::free(p);

        let tl = dd_tl_state_get();
        if !tl.is_null() {
            (*tl).sampling_interval = 512 * 1024;
        }
    }
}
