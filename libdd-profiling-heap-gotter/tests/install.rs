// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end smoke tests: install the GOT overrides into the live test
//! process, exercise libc allocator functions, and confirm the sampler
//! is actually intercepting them.
//!
//! These tests mutate global process state (GOT entries), so they must
//! not run in parallel with each other. The `#[serial]` attribute
//! enforces that.
//!
//! Installation is permanent - there is no un-install - so a test can't
//! return the process to a pristine state afterward. Isolation therefore
//! relies on nextest running each test in its own process (which CI does,
//! including the coverage job). Under a shared-process runner (`cargo
//! test`) a prior test's install would leak into later ones.

// Integration tests invoke GOT-patching machinery for real
// (dl_iterate_phdr + mprotect), which miri can't execute.
#![cfg(all(target_os = "linux", target_pointer_width = "64", not(miri)))]

use std::ffi::c_void;

use libdd_profiling_heap_sampler::{dd_sample_flag_peek, dd_tl_state_get_or_init};
use serial_test::serial;

/// Warn (once) when these tests aren't running under nextest. The GOT install
/// is permanent, so isolation depends on each test getting its own process;
/// nextest sets `NEXTEST=1`, plain `cargo test` shares one process and leaks
/// a prior test's install into later ones.
fn warn_if_not_isolated() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("NEXTEST").is_none() {
            eprintln!(
                "warning: heap-gotter install tests need per-test process isolation; \
                 run with `cargo nextest run`. Under `cargo test` a prior test's \
                 permanent GOT install leaks into later ones."
            );
        }
    });
}

/// After install the heap should still be functional and no recursive
/// crash should occur when malloc/free go through the patched GOT.
#[test]
#[serial]
fn install_keeps_heap_functional() {
    warn_if_not_isolated();
    extern "C" {
        fn malloc(size: usize) -> *mut c_void;
    }

    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
    assert!(
        installed,
        "expected install_heap_overrides to find at least one symbol"
    );

    unsafe {
        let p = malloc(64) as *mut u8;
        assert!(!p.is_null(), "malloc returned NULL post-install");
        // Write then read back the whole buffer to prove it's real, usable
        // memory, not just a non-NULL (possibly mis-offset) pointer.
        for i in 0..64 {
            p.add(i).write((i as u8) ^ 0xA5);
        }
        for i in 0..64 {
            assert_eq!(p.add(i).read(), (i as u8) ^ 0xA5, "byte {i} corrupted");
        }
        libc::free(p as *mut c_void);
    }
}

/// Confirm that after install, allocations actually flow through the
/// sampler and produce tagged pointers. We force sampling_interval=1
/// so every allocation is sampled, then check that libc::malloc returns
/// a pointer carrying the sample flag.
#[cfg(feature = "live-heap")]
#[test]
#[serial]
fn install_produces_sampled_allocations() {
    warn_if_not_isolated();
    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
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

        // Use peek (non-destructive) to verify the flag is set without
        // clearing it. gotter_free needs the flag intact to recover the
        // raw pointer.
        let mut raw: *mut c_void = std::ptr::null_mut();
        let mut offset: usize = 0;
        let sampled = dd_sample_flag_peek(p, &mut raw, &mut offset);
        assert!(
            sampled,
            "expected malloc to return a sampled pointer with interval=1"
        );
        assert!(!raw.is_null());

        // Free via libc::free which goes through gotter_free; it
        // handles the tagged pointer correctly (check + free raw).
        libc::free(p);
    }
}

/// Confirm realloc(NULL, size) goes through the sampler-side allocation
/// case, not a gotter-specific special case.
#[cfg(feature = "live-heap")]
#[test]
#[serial]
fn realloc_null_produces_sampled_allocation() {
    warn_if_not_isolated();
    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;

        let p = libc::realloc(std::ptr::null_mut(), 128);
        assert!(!p.is_null());

        let mut raw: *mut c_void = std::ptr::null_mut();
        let mut offset: usize = 0;
        let sampled = dd_sample_flag_peek(p, &mut raw, &mut offset);
        assert!(
            sampled,
            "realloc(NULL, size) should use allocation sampling"
        );

        libc::free(p);
    }
}

/// On x86-64, page-aligned allocations must pass through unsampled.
/// The header checker refuses pointers in the first 16 bytes of a page,
/// so a sampled 4096-aligned pointer could not be recognised later by
/// free or realloc.
#[cfg(all(target_arch = "x86_64", feature = "live-heap"))]
#[test]
#[serial]
fn page_aligned_allocations_are_unsampled() {
    warn_if_not_isolated();
    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;

        let p = libc::aligned_alloc(4096, 4096);
        assert!(!p.is_null());
        assert_eq!((p as usize) % 4096, 0);

        let mut raw: *mut c_void = std::ptr::null_mut();
        let mut offset: usize = 0;
        let sampled = dd_sample_flag_peek(p, &mut raw, &mut offset);
        assert!(
            !sampled,
            "page-aligned allocation must pass through unsampled"
        );

        libc::free(p);
    }
}

/// Same as above but for realloc: confirm a sampled allocation that
/// gets reallocated comes back as a valid (unsampled) pointer with the
/// user data intact.
#[test]
#[serial]
fn realloc_of_sampled_allocation_preserves_data() {
    warn_if_not_isolated();
    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
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
        // valid and the original data should be preserved.
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
    }
}

/// Allocate `size` bytes at `align` through the (hooked) libc. Uses
/// posix_memalign for real alignments and malloc for align <= 1;
/// posix_memalign requires alignment to be a power-of-two multiple of
/// sizeof(void*), which every alignment in the menu except 1 satisfies,
/// and (unlike aligned_alloc) it places no multiple-of-alignment
/// constraint on the size.
unsafe fn alloc_aligned(align: usize, size: usize) -> *mut c_void {
    if align <= 1 {
        libc::malloc(size)
    } else {
        let mut out: *mut c_void = std::ptr::null_mut();
        if libc::posix_memalign(&mut out, align, size) != 0 {
            std::ptr::null_mut()
        } else {
            out
        }
    }
}

/// Stress the realloc + free paths across a matrix of alignments and
/// sizes with sampling forced on. Mirrors examples/gotter_usdt_demo.rs's
/// allocation menu, which straddles DD_SAMPLE_ALIGNMENT_CAP (1024 == cap,
/// 2048/4096/8192 above it), and is a regression guard for the class of
/// crash where a sampled, page-aligned pointer is mis-recovered on
/// free/realloc and an invalid pointer is handed to the real allocator
/// (munmap_chunk(): invalid pointer / SIGABRT).
///
/// The headline assertion is that the whole matrix completes without
/// aborting; per-iteration checks confirm the surviving prefix is
/// preserved across each realloc, and `saw_sampled` confirms the tagged
/// pointer / raw-recovery path was actually exercised (so a regression
/// that silently stopped sampling can't make this pass trivially).
#[test]
#[serial]
fn realloc_stress_across_alignments_preserves_data() {
    warn_if_not_isolated();
    // Mirrors the demo's menu, plus 2048 to bracket the 1024 cap on both
    // sides. Small alignments sample; those above the cap pass through.
    const ALIGNMENTS: &[usize] = &[1, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];
    // A realloc walk per allocation: grow, grow, shrink, grow.
    const SIZE_WALK: &[usize] = &[48, 200, 1000, 64, 4096];

    // Deterministic, per-alignment-distinct pattern so we can verify the
    // surviving prefix after each realloc. Pure arithmetic - no allocation.
    fn pattern_byte(seed: usize, i: usize) -> u8 {
        (i as u8)
            .wrapping_mul(31)
            .wrapping_add((seed as u8).wrapping_mul(101))
            ^ 0x5A
    }

    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
    assert!(installed);

    let mut saw_sampled = false;

    unsafe {
        let tl = dd_tl_state_get_or_init();
        assert!(!tl.is_null());
        // Force every allocation to sample so the tagged-pointer /
        // raw-recovery paths are exercised on every eligible iteration.
        // Everything below this point avoids incidental Rust allocation
        // (const inputs, raw-pointer writes only) so the sampled window
        // isn't polluted by the test's own bookkeeping.
        (*tl).sampling_interval = 1;
        (*tl).remaining_bytes = 0;
        (*tl).remaining_bytes_initialized = true;

        for (seed, &align) in ALIGNMENTS.iter().enumerate() {
            let mut size = SIZE_WALK[0];

            let mut p = alloc_aligned(align, size) as *mut u8;
            assert!(!p.is_null(), "alloc failed (align={align}, size={size})");

            // Record whether this block was actually sampled (peek is
            // non-destructive, so it's safe before the realloc walk).
            let mut raw: *mut c_void = std::ptr::null_mut();
            let mut offset: usize = 0;
            if dd_sample_flag_peek(p as *mut c_void, &mut raw, &mut offset) {
                saw_sampled = true;
            }

            for i in 0..size {
                *p.add(i) = pattern_byte(seed, i);
            }

            for &new_size in &SIZE_WALK[1..] {
                let p2 = libc::realloc(p as *mut c_void, new_size) as *mut u8;
                assert!(
                    !p2.is_null(),
                    "realloc failed (align={align}, {size}->{new_size})"
                );

                // The overlapping prefix must survive the realloc.
                let preserved = size.min(new_size);
                for i in 0..preserved {
                    assert_eq!(
                        *p2.add(i),
                        pattern_byte(seed, i),
                        "corruption at byte {i} (align={align}, {size}->{new_size})"
                    );
                }
                // Repaint the whole new region for the next realloc to keep.
                for i in 0..new_size {
                    *p2.add(i) = pattern_byte(seed, i);
                }

                p = p2;
                size = new_size;
            }

            libc::free(p as *mut c_void);
        }
    }

    // Only meaningful with live-heap tracking on; without it nothing is
    // flagged, so the realloc/free stress still exercises the passthrough
    // paths but no allocation is ever "sampled".
    #[cfg(feature = "live-heap")]
    assert!(
        saw_sampled,
        "expected at least one allocation to be sampled with interval=1"
    );
    #[cfg(not(feature = "live-heap"))]
    let _ = saw_sampled;
}

/// A failing allocation must surface the real allocator's error/errno through
/// our hooks unchanged. Avoids relying on `aligned_alloc` rejecting a bad
/// alignment, since older glibc (e.g. CentOS 7) doesn't validate that.
#[test]
#[serial]
fn invalid_alignment_passes_through_error() {
    warn_if_not_isolated();
    let installed = libdd_profiling_heap_gotter::install_heap_overrides();
    assert!(installed);

    unsafe {
        // posix_memalign with a non-power-of-two alignment must return EINVAL.
        // POSIX-specified, so reliable across glibc versions; exercises our
        // hook forwarding the real return code unchanged.
        let mut out: *mut c_void = std::ptr::null_mut();
        let rc = libc::posix_memalign(&mut out, 3, 64);
        assert_eq!(rc, libc::EINVAL, "posix_memalign should return EINVAL");

        // A failing allocation must preserve the real allocator's errno through
        // our post-call bookkeeping. A huge aligned_alloc reliably fails with
        // ENOMEM on every libc (size is a multiple of the alignment).
        *libc::__errno_location() = 0;
        let huge = usize::MAX & !0xfff;
        let p = libc::aligned_alloc(16, huge);
        assert!(p.is_null(), "huge aligned_alloc should fail");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOMEM),
            "errno must survive our post-call bookkeeping"
        );
    }
}
