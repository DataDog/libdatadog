// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! An `LD_PRELOAD`-able shared object that makes `jemalloc` the process
//! allocator *and* turns on `libdd-profiling-heap-jemalloc`'s sampling hooks —
//! for programs you can't (or don't want to) recompile with
//! `#[global_allocator]` + a call to
//! [`libdd_profiling_heap_jemalloc::install`].
//!
//! ```text
//! LD_PRELOAD=/path/to/libdd_profiling_heap_jemalloc_preload.so ./your-program
//! ```
//!
//! # How it works
//!
//! This crate builds as a `cdylib` around two small pieces, both Linux-only:
//!
//! 1. **Allocator interposition.** It links the (prefixed) `jemalloc` from
//!    `tikv-jemalloc-sys` and re-exports the libc allocator entry points
//!    (`malloc`, `free`, ...) as thin `#[no_mangle]` forwarders to
//!    `jemalloc`'s prefixed symbols. Those forwarders — not `jemalloc`'s own
//!    symbols — are what the dynamic linker binds a preloaded program's
//!    `malloc`/`free` to. (A plain `cdylib` can't just re-export the
//!    statically-linked `jemalloc` symbols directly: `rustc` emits a version
//!    script that marks every non-Rust symbol local, so they'd be hidden.
//!    The forwarders sidestep that because they're `#[no_mangle] pub` Rust
//!    symbols that `rustc` does export.) No allocator logic is reimplemented
//!    here; each shim is a one-line hand-off.
//!
//! 2. **Hook installation at load.** An `.init_array` constructor calls
//!    [`libdd_profiling_heap_jemalloc::install`] as the library loads, so the
//!    process starts sampling with no code change on its side.
//!
//! # Configuration
//!
//! Sampling honours the same knobs as `libdd-profiling-heap-jemalloc`:
//! `install` resets `jemalloc` to the 512 KiB default interval and flips
//! `prof.active` on, and `DD_HEAP_SAMPLING_ENABLED=0` (etc.) disables the
//! whole thing (`install` becomes a no-op). The `prof:true,prof_active:false`
//! `MALLOC_CONF` this needs is baked into the `tikv-jemalloc-sys` build by its
//! `profiling_hooks` feature.

// Everything below is Linux-only; see the crate docs. On other targets this is
// an empty cdylib. The module is `pub` so the `#[no_mangle]` forwarders count
// as reachable and land in the cdylib's export list — a `#[no_mangle]` symbol
// inside a private module is hidden by the version script `rustc` generates.
#[cfg(target_os = "linux")]
pub mod preload {
    // Each forwarder's safety contract is exactly the C library contract of
    // the libc function it shadows (see the module docs) — they exist only to
    // be bound by the dynamic linker as `malloc`/`free`/etc., never called
    // from Rust — so a per-function `# Safety` stanza would just restate that
    // seven times.
    #![allow(clippy::missing_safety_doc)]

    use std::os::raw::{c_int, c_void};

    // Thin forwarders from the libc allocator surface to the prefixed jemalloc
    // symbols `tikv-jemalloc-sys` exposes. These are the symbols a preloaded
    // program actually binds to; each just hands off, no logic of its own.
    #[no_mangle]
    pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        tikv_jemalloc_sys::malloc(size)
    }

    #[no_mangle]
    pub unsafe extern "C" fn calloc(number: usize, size: usize) -> *mut c_void {
        tikv_jemalloc_sys::calloc(number, size)
    }

    #[no_mangle]
    pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        tikv_jemalloc_sys::realloc(ptr, size)
    }

    #[no_mangle]
    pub unsafe extern "C" fn free(ptr: *mut c_void) {
        tikv_jemalloc_sys::free(ptr)
    }

    #[no_mangle]
    pub unsafe extern "C" fn posix_memalign(
        ptr: *mut *mut c_void,
        alignment: usize,
        size: usize,
    ) -> c_int {
        tikv_jemalloc_sys::posix_memalign(ptr, alignment, size)
    }

    #[no_mangle]
    pub unsafe extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void {
        tikv_jemalloc_sys::aligned_alloc(alignment, size)
    }

    #[no_mangle]
    pub unsafe extern "C" fn malloc_usable_size(ptr: *const c_void) -> usize {
        tikv_jemalloc_sys::malloc_usable_size(ptr)
    }

    // Run `install()` as the library loads — no cooperation needed from the
    // preloaded program. `.init_array` is exactly what the `ctor` crate
    // expands to; we inline it to avoid the extra dependency. `#[used]` keeps
    // the linker from dropping the otherwise-unreferenced entry.
    #[used]
    #[link_section = ".init_array"]
    static DD_HEAP_JEMALLOC_PRELOAD_INIT: extern "C" fn() = dd_heap_jemalloc_preload_init;

    extern "C" fn dd_heap_jemalloc_preload_init() {
        // Errors (e.g. profiling compiled out) are intentionally swallowed: a
        // preloaded allocator must never abort the host process just because
        // sampling couldn't be turned on.
        let _ = libdd_profiling_heap_jemalloc::install();
    }
}
