// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! What only the C artifact (`--cfg ddprof_ffi_cdylib`) needs: a panic
//! handler that must never be linked in, and a global allocator.

use core::alloc::{GlobalAlloc, Layout};
#[cfg(unix)]
use core::ffi::c_void;
use core::ptr;

#[cfg(not(ddprof_ffi_diagnose))]
unsafe extern "C" {
    /// Deliberately undefined. If any panic path survives optimization, the
    /// panic handler below references this symbol and linking the cdylib
    /// fails (or `nm` shows it): that is the no-panic check.
    fn __ddog_prof_panic_is_reachable() -> !;
}

#[cfg(not(ddprof_ffi_diagnose))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: never actually linked; see above.
    unsafe { __ddog_prof_panic_is_reachable() }
}

/// The diagnostic build's handler: a real function, so the library links
/// and its callers can be found by disassembling it.
#[cfg(ddprof_ffi_diagnose)]
#[panic_handler]
#[inline(never)]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// The C allocator. Failure returns null, which the library reports as an
/// out-of-memory error rather than aborting.
struct Malloc;

/// `malloc` alignment that needs no `posix_memalign` (`_aligned_malloc` on
/// Windows): 16 on every 64-bit target this library supports.
const MIN_ALIGN: usize = 16;

/// An over-aligned block (`align > MIN_ALIGN`), or null.
///
/// # Safety
///
/// `align` is a power of two.
unsafe fn alloc_aligned(size: usize, align: usize) -> *mut u8 {
    #[cfg(unix)]
    {
        let mut out: *mut c_void = ptr::null_mut();
        // SAFETY: `align` is a power of two and a multiple of the pointer size.
        if unsafe { libc::posix_memalign(&raw mut out, align, size) } == 0 {
            out.cast()
        } else {
            ptr::null_mut()
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: `align` is a power of two.
        unsafe { libc::aligned_malloc(size, align).cast() }
    }
}

/// Frees a block from [`alloc_aligned`].
///
/// # Safety
///
/// `block` came from [`alloc_aligned`] and is not used again.
unsafe fn free_aligned(block: *mut u8) {
    // SAFETY: the caller's contract; on Unix, posix_memalign blocks are
    // freed with free.
    unsafe {
        #[cfg(unix)]
        libc::free(block.cast());
        #[cfg(windows)]
        libc::aligned_free(block.cast());
    }
}

// SAFETY: forwards to the C allocator, honoring size and alignment.
unsafe impl GlobalAlloc for Malloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded to the C runtime with a valid size; a layout's
        // alignment is a power of two.
        unsafe {
            if layout.align() <= MIN_ALIGN {
                libc::malloc(layout.size()).cast()
            } else {
                alloc_aligned(layout.size(), layout.align())
            }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MIN_ALIGN {
            // SAFETY: forwarded to libc.
            return unsafe { libc::calloc(1, layout.size()).cast() };
        }
        // SAFETY: as for `alloc`; the block holds `size` bytes.
        unsafe {
            let block = self.alloc(layout);
            if !block.is_null() {
                ptr::write_bytes(block, 0, layout.size());
            }
            block
        }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: the block came from this allocator with `layout`, so its
        // alignment says which function allocated it.
        unsafe {
            if layout.align() <= MIN_ALIGN {
                libc::free(block.cast());
            } else {
                free_aligned(block);
            }
        }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the block came from this allocator with `layout`.
        unsafe {
            if layout.align() <= MIN_ALIGN {
                return libc::realloc(block.cast(), new_size).cast();
            }
            let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
                return ptr::null_mut();
            };
            let new = self.alloc(new_layout);
            if !new.is_null() {
                ptr::copy_nonoverlapping(block, new, layout.size().min(new_size));
                free_aligned(block);
            }
            new
        }
    }
}

#[global_allocator]
static ALLOCATOR: Malloc = Malloc;
