// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT hook functions and their per-symbol "real" function pointer slots.
//!
//! Each `gotter_*` function:
//! 1. Calls `dd_allocation_requested` (or skips, for free-side hooks).
//! 2. Forwards to the real symbol via its `ORIG_*` slot, which the install path fills in by
//!    symbol-table lookup.
//! 3. Calls `dd_allocation_created` / `dd_allocation_freed` to fire the USDT.
//!
//! Modeled on ddprof `src/lib/symbol_overrides.cc`, minus the C++ allocator
//! family (operator new/delete) and the mmap/munmap pair which aren't
//! supported by the sampler yet.

use core::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use libdd_profiling_heap_sampler::{
    dd_alloc_req_t, dd_allocation_created, dd_allocation_freed, dd_allocation_realloc_commit,
    dd_allocation_realloc_prepare, dd_allocation_requested, dd_tl_state_init,
};

/// Resolved address of the real `malloc`; filled by `install_heap_overrides`.
/// The bare `usize` payload is a function pointer.
pub(crate) static ORIG_MALLOC: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `free`.
pub(crate) static ORIG_FREE: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `calloc`.
pub(crate) static ORIG_CALLOC: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `realloc`.
pub(crate) static ORIG_REALLOC: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `posix_memalign`.
pub(crate) static ORIG_POSIX_MEMALIGN: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `aligned_alloc`.
pub(crate) static ORIG_ALIGNED_ALLOC: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `dlopen`.
pub(crate) static ORIG_DLOPEN: AtomicUsize = AtomicUsize::new(0);
/// Resolved address of the real `pthread_create`.
pub(crate) static ORIG_PTHREAD_CREATE: AtomicUsize = AtomicUsize::new(0);

/// Counts hook invocations so integration tests outside this crate can
/// prove the patched GOT was actually exercised, not just that nothing
/// crashed. Only `malloc`/`free` increment it: that's enough to prove the
/// hooks ran without adding bookkeeping to every symbol.
#[cfg(feature = "test-support")]
pub(crate) static HOOK_HITS: AtomicUsize = AtomicUsize::new(0);

/// Load a resolved function pointer from one of the `ORIG_*` slots.
///
/// # Safety
///
/// `T` must be exactly the `extern "C" fn(...)` pointer type that
/// `apply_overrides` writes into `slot` (see the `ORIG_*` docs above); if
/// `slot` is still `0`, this returns `None` rather than transmuting.
#[inline]
unsafe fn load_fn<T>(slot: &AtomicUsize) -> Option<T> {
    // `Acquire` pairs with the `store(Release)` in elf.rs's
    // `apply_overrides`, which runs before the GOT is ever patched to
    // route calls into these hooks. That gives a real happens-before
    // edge: once this observes a non-zero slot, it's guaranteed to see
    // the fully-published address, not just "no torn read".
    let v = slot.load(Ordering::Acquire);
    if v == 0 {
        None
    } else {
        // SAFETY: caller guarantees T is the right `extern "C" fn(...)`
        // type and that `v` was written by `apply_overrides` with a
        // function of that signature.
        //
        // transmute_copy, not transmute: `transmute::<usize, T>` won't
        // compile for a generic T because the size-equality check runs
        // before monomorphization, so the compiler can't prove
        // `size_of::<T>() == size_of::<usize>()`. Every T here is a
        // pointer-sized fn pointer, so copying `size_of::<T>()` bytes is
        // sound.
        Some(core::mem::transmute_copy::<usize, T>(&v))
    }
}

// These mirror the C-standard / POSIX ABI signatures of the hooked
// functions (malloc(size_t), free(void*), ...). That ABI is effectively
// frozen, and a C function's signature can't be introspected at runtime,
// so there is no runtime guard against drift - correctness relies on the
// standardized ABI staying put.
/// Signature of the real `malloc`.
type MallocFn = unsafe extern "C" fn(usize) -> *mut c_void;
/// Signature of the real `free`.
type FreeFn = unsafe extern "C" fn(*mut c_void);
/// Signature of the real `calloc`.
type CallocFn = unsafe extern "C" fn(usize, usize) -> *mut c_void;
/// Signature of the real `realloc`.
type ReallocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
/// Signature of the real `posix_memalign`.
type PosixMemalignFn = unsafe extern "C" fn(*mut *mut c_void, usize, usize) -> c_int;
/// Signature of the real `aligned_alloc`.
type AlignedAllocFn = unsafe extern "C" fn(usize, usize) -> *mut c_void;
/// Signature of the real `dlopen`.
type DlopenFn = unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void;
/// Linux RTLD_DEEPBIND. Some libcs we build against (notably musl on Alpine)
/// don't expose this constant through the Rust libc crate, but the flag value
/// is stable Linux ABI from <dlfcn.h>.
const RTLD_DEEPBIND: c_int = 0x00008;
/// Signature of a `pthread_create` start routine.
type StartRoutine = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
/// Signature of the real `pthread_create`.
type PthreadCreateFn = unsafe extern "C" fn(
    *mut libc::pthread_t,
    *const libc::pthread_attr_t,
    StartRoutine,
    *mut c_void,
) -> c_int;

#[no_mangle]
pub unsafe extern "C" fn gotter_malloc(size: usize) -> *mut c_void {
    #[cfg(feature = "test-support")]
    HOOK_HITS.fetch_add(1, Ordering::Relaxed);
    let Some(real): Option<MallocFn> = load_fn(&ORIG_MALLOC) else {
        return std::ptr::null_mut();
    };
    // Default alignment for malloc on glibc is 2*sizeof(void*) == 16.
    let req = dd_allocation_requested(size, core::mem::align_of::<*mut c_void>() * 2);
    let raw = real(req.size);
    dd_allocation_created(raw, req)
}

#[no_mangle]
pub unsafe extern "C" fn gotter_free(ptr: *mut c_void) {
    #[cfg(feature = "test-support")]
    HOOK_HITS.fetch_add(1, Ordering::Relaxed);
    let Some(real): Option<FreeFn> = load_fn(&ORIG_FREE) else {
        return;
    };
    // Forward unconditionally, including free(NULL): the sampler's check
    // rejects NULL without dereferencing (returns it unchanged), and
    // free(NULL) is a defined no-op - forwarding preserves whatever the
    // real allocator does rather than assuming.
    let freed = dd_allocation_freed(ptr, 0, 0);
    real(freed.ptr);
}

#[no_mangle]
pub unsafe extern "C" fn gotter_calloc(nmemb: usize, size: usize) -> *mut c_void {
    let Some(real): Option<CallocFn> = load_fn(&ORIG_CALLOC) else {
        return std::ptr::null_mut();
    };
    let Some(total) = nmemb.checked_mul(size) else {
        return real(nmemb, size);
    };
    let req = dd_allocation_requested(total, core::mem::align_of::<*mut c_void>() * 2);
    // calloc takes (nmemb, size); when the sampler bumps `req.size` we
    // funnel the extra bytes into the size argument (nmemb stays 1's
    // worth conceptually). The simplest robust path is to switch to a
    // single (1, req.size) allocation when sampling kicks in, so the
    // underlying allocator zeroes everything we hand back. Unsampled
    // path keeps the user's (nmemb, size) verbatim.
    // `req.weight == 0` is `!dd_alloc_req_is_sampled(req)`, inlined here to avoid a cross-FFI call.
    let raw = if req.weight == 0 {
        real(nmemb, size)
    } else {
        real(1, req.size)
    };
    dd_allocation_created(raw, req)
}

/// `realloc` hook.
///
/// The sampler owns the realloc cases and pointer math. Gotter only does
/// the pre/post split: ask the sampler what raw call to make, call the
/// real realloc symbol, then ask the sampler to turn the result back into
/// the user-visible pointer.
#[no_mangle]
pub unsafe extern "C" fn gotter_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    let Some(real): Option<ReallocFn> = load_fn(&ORIG_REALLOC) else {
        return std::ptr::null_mut();
    };
    let prep = dd_allocation_realloc_prepare(ptr, size);
    let new_raw = real(prep.raw_ptr, prep.raw_size);
    dd_allocation_realloc_commit(ptr, new_raw, prep)
}

#[no_mangle]
pub unsafe extern "C" fn gotter_posix_memalign(
    memptr: *mut *mut c_void,
    alignment: usize,
    size: usize,
) -> c_int {
    let Some(real): Option<PosixMemalignFn> = load_fn(&ORIG_POSIX_MEMALIGN) else {
        return libc::ENOMEM;
    };
    let req = dd_allocation_requested(size, alignment);
    let ret = real(memptr, alignment, req.size);
    // Always pair with dd_allocation_created, even on failure, so the
    // sampler's reentry guard opened by dd_allocation_requested is
    // closed. Passing raw == NULL is the documented "allocation
    // failed" path: no flag stamped, no USDT fired, guard closed.
    if ret == 0 && !memptr.is_null() {
        let raw = *memptr;
        *memptr = dd_allocation_created(raw, req);
    } else {
        let _ = dd_allocation_created(std::ptr::null_mut(), req);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn gotter_aligned_alloc(alignment: usize, size: usize) -> *mut c_void {
    let Some(real): Option<AlignedAllocFn> = load_fn(&ORIG_ALIGNED_ALLOC) else {
        return std::ptr::null_mut();
    };
    let req = dd_allocation_requested(size, alignment);
    let raw = real(alignment, req.size);
    dd_allocation_created(raw, req)
}

/// Forward `dlopen` and then patch any GOT entries introduced by the newly-loaded library.
#[no_mangle]
pub unsafe extern "C" fn gotter_dlopen(filename: *const c_char, flags: c_int) -> *mut c_void {
    let Some(real): Option<DlopenFn> = load_fn(&ORIG_DLOPEN) else {
        // Hooks not yet wired up; calling real() would NPE - punt to libc.
        return libc::dlopen(filename, flags);
    };
    let handle = real(filename, flags);
    if flags & RTLD_DEEPBIND != 0 {
        // DEEPBIND changes symbol resolution order and causes issues with
        // GOT patching, so skip newly-loaded deep-bound libraries for now.
        return handle;
    }
    // New library may have introduced new GOT entries that need patching.
    // This hook is an extern "C" boundary, so never let a Rust panic from
    // best-effort ELF parsing/GOT patching unwind into the caller.
    let _ = std::panic::catch_unwind(crate::update_heap_overrides);
    handle
}

/// Args we package up so the wrapped start routine sees its original
/// arg through our trampoline.
struct PthreadCreateArgs {
    start: StartRoutine,
    arg: *mut c_void,
}

unsafe extern "C" fn pthread_start_trampoline(arg: *mut c_void) -> *mut c_void {
    let boxed: Box<PthreadCreateArgs> = Box::from_raw(arg as *mut PthreadCreateArgs);
    // Materialise per-thread sampler state up front so the first
    // tracked alloc on this thread doesn't have to.
    dd_tl_state_init();
    (boxed.start)(boxed.arg)
}

#[no_mangle]
pub unsafe extern "C" fn gotter_pthread_create(
    thread: *mut libc::pthread_t,
    attr: *const libc::pthread_attr_t,
    start: StartRoutine,
    arg: *mut c_void,
) -> c_int {
    let Some(real): Option<PthreadCreateFn> = load_fn(&ORIG_PTHREAD_CREATE) else {
        return libc::EAGAIN;
    };
    let boxed = Box::new(PthreadCreateArgs { start, arg });
    let raw = Box::into_raw(boxed);
    let rc = real(thread, attr, pthread_start_trampoline, raw as *mut c_void);
    if rc != 0 {
        // Reclaim the box; trampoline won't run.
        drop(Box::from_raw(raw));
    }
    rc
}

// Touch the sampler-side reentry guard helpers indirectly; silences
// "unused import" warnings about `dd_alloc_req_t` once cfg-gated paths
// extend hooks later.
#[allow(dead_code)]
fn _types_anchor() -> dd_alloc_req_t {
    dd_alloc_req_t {
        size: 0,
        user_size: 0,
        alignment: 0,
        weight: 0,
    }
}
