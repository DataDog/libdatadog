// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT hook functions and their per-symbol "real" function pointer slots.
//!
//! Each `gotter_*` function:
//! 1. Calls `dd_allocation_requested` (or skips, for free-side hooks).
//! 2. Forwards to the real symbol via its `ORIG_*` slot, which the install path fills in by
//!    symbol-table lookup.
//! 3. Calls `dd_allocation_created` / `dd_allocation_freed` to fire the USDT and close the reentry
//!    guard.
//!
//! Modeled on ddprof `src/lib/symbol_overrides.cc`, minus the C++ allocator
//! family (operator new/delete) and the mmap/munmap pair which aren't
//! supported by the sampler yet.

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use libdd_heap_sampler::{
    dd_alloc_req_t, dd_allocation_created, dd_allocation_freed, dd_allocation_requested,
    dd_probe_free, dd_sample_flag_peek, dd_tl_state_get, dd_tl_state_init,
};

use crate::realloc_math::sampled_realloc_raw_size;

// Per-thread reentry guard for the gotter shims themselves. Distinct
// from the sampler's `dd_tl_state_t::reentry_guard` because that one
// lives inside a struct we have to *allocate* (via `calloc`) on first
// touch - and on a freshly-installed gotter that first `calloc` lands
// right back in `gotter_calloc`. So before we look at the sampler TLS
// at all, we set this flag; any reentry while it's set forwards
// straight through to the real allocator with no sampling.
//
// `const { Cell::new(false) }` keeps the TLS slot lazy-init-free -
// macOS and glibc both initialise it without an allocation.
std::thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

struct GotterReentry(bool);

impl GotterReentry {
    fn enter() -> Self {
        let was = IN_HOOK.with(|c| {
            let prev = c.get();
            c.set(true);
            prev
        });
        GotterReentry(was)
    }
    fn reentered(&self) -> bool {
        self.0
    }
}

impl Drop for GotterReentry {
    fn drop(&mut self) {
        IN_HOOK.with(|c| c.set(self.0));
    }
}

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

/// Ensure the sampler's per-thread state exists before recording an allocation.
#[inline]
fn ensure_tls() {
    unsafe {
        if dd_tl_state_get().is_null() {
            // dd_tl_state_init calls calloc internally; the gotter
            // reentry guard around the caller stops us from re-entering
            // this path through our own gotter_calloc hook.
            dd_tl_state_init();
        }
    }
}

/// Load a resolved function pointer from one of the `ORIG_*` slots.
#[inline]
unsafe fn load_fn<T>(slot: &AtomicUsize) -> Option<T> {
    let v = slot.load(Ordering::Acquire);
    if v == 0 {
        None
    } else {
        // SAFETY: caller guarantees T is the right `extern "C" fn(...)`
        // type and that `v` was written by `apply_overrides` with a
        // function of that signature.
        Some(core::mem::transmute_copy::<usize, T>(&v))
    }
}

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
    let Some(real): Option<MallocFn> = load_fn(&ORIG_MALLOC) else {
        return std::ptr::null_mut();
    };
    let guard = GotterReentry::enter();
    if guard.reentered() {
        return real(size);
    }
    ensure_tls();
    // Default alignment for malloc on glibc is 2*sizeof(void*) == 16.
    let req = dd_allocation_requested(size, core::mem::align_of::<u64>() * 2);
    let raw = real(req.size);
    dd_allocation_created(raw, req)
}

#[no_mangle]
pub unsafe extern "C" fn gotter_free(ptr: *mut c_void) {
    let Some(real): Option<FreeFn> = load_fn(&ORIG_FREE) else {
        return;
    };
    if ptr.is_null() {
        return;
    }
    let guard = GotterReentry::enter();
    if guard.reentered() {
        real(ptr);
        return;
    }
    let freed = dd_allocation_freed(ptr, 0, 0);
    real(freed.ptr);
}

#[no_mangle]
pub unsafe extern "C" fn gotter_calloc(nmemb: usize, size: usize) -> *mut c_void {
    let Some(real): Option<CallocFn> = load_fn(&ORIG_CALLOC) else {
        return std::ptr::null_mut();
    };
    let guard = GotterReentry::enter();
    if guard.reentered() {
        return real(nmemb, size);
    }
    ensure_tls();
    let total = nmemb.saturating_mul(size);
    let req = dd_allocation_requested(total, core::mem::align_of::<u64>() * 2);
    // calloc takes (nmemb, size); when the sampler bumps `req.size` we
    // funnel the extra bytes into the size argument (nmemb stays 1's
    // worth conceptually). The simplest robust path is to switch to a
    // single (1, req.size) allocation when sampling kicks in, so the
    // underlying allocator zeroes everything we hand back. Unsampled
    // path keeps the user's (nmemb, size) verbatim.
    let raw = if req.weight == 0 {
        real(nmemb, size)
    } else {
        real(1, req.size)
    };
    dd_allocation_created(raw, req)
}

/// `realloc` hook.
///
/// Handled as four disjoint cases:
///
/// 1. **`ptr == NULL`** — equivalent to `malloc(size)`. Runs the normal sampling path (`request` +
///    `created`).
/// 2. **`size == 0`** — equivalent to `free(ptr)` for the allocators we hook. Consumes the sampler
///    flag via `dd_allocation_freed` (clears the header + fires `ddheap:free`) and forwards.
/// 3. **`ptr` is unsampled** — passthrough to the underlying realloc. We don't newly sample here
///    (see TODO on the branch).
/// 4. **`ptr` is sampled** — MVP: model a successful sampled realloc as `free(old sampled)`
///    followed by a new *unsampled* allocation. See the branch comment for the full rationale.
#[no_mangle]
pub unsafe extern "C" fn gotter_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    let Some(real): Option<ReallocFn> = load_fn(&ORIG_REALLOC) else {
        return std::ptr::null_mut();
    };
    let guard = GotterReentry::enter();
    if guard.reentered() {
        return real(ptr, size);
    }
    ensure_tls();

    // Case 1: realloc(NULL, size) == malloc(size). Normal sampling path.
    if ptr.is_null() {
        let alignment = core::mem::align_of::<u64>() * 2;
        let req = dd_allocation_requested(size, alignment);
        let raw = real(std::ptr::null_mut(), req.size);
        return dd_allocation_created(raw, req);
    }

    // Case 2: realloc(p, 0) == free(p). Safe to consume the sampler
    // flag before forwarding because the old block will not remain
    // live on this path.
    if size == 0 {
        let freed = dd_allocation_freed(ptr, 0, 0);
        return real(freed.ptr, 0);
    }

    // Peek is non-destructive: if realloc fails below, the old header
    // must stay intact so a later gotter_free(ptr) still resolves the
    // right raw pointer for libc.
    let mut old_raw = std::ptr::null_mut();
    let mut old_offset = 0usize;
    if !dd_sample_flag_peek(ptr, &mut old_raw, &mut old_offset) {
        // Case 3: unsampled old allocation. Passthrough. We deliberately
        // do not opportunistically sample here: we don't know the old
        // user-requested size, so we can't safely place a header and
        // move user contents without either over-reading the old block
        // or shifting user data.
        // TODO: revisit once sampled headers carry the original user
        // size, or we have another safe bound for copying old contents.
        return real(ptr, size);
    }

    // Case 4: sampled old allocation. MVP semantics:
    //
    //   successful sampled realloc = ddheap:free(old sampled)
    //                              + new unsampled allocation
    //
    // Why unsampled on the new side:
    //   * The new raw pointer picked by the underlying realloc may sit at a different page offset,
    //     so the sampler's x86 offset picker may want a different `new_offset` than `old_offset`.
    //     Stamping a header at the new offset before moving user data would overlap the
    //     still-to-be-copied source region.
    //   * We also don't know the original user-requested size, so we can't cheaply resize +
    //     resample + report a coherent alloc event to the profiler.
    //
    // The overhead of "unsampled from now on" is only paid on
    // sampled-then-realloc'd blocks, which are already rare.
    //
    // TODO: emit `free + new sampled allocation` when sampled headers
    // carry the original user size (see project TODO #13 / repo TODO #7).
    let Some(raw_size) = sampled_realloc_raw_size(size, old_offset) else {
        return std::ptr::null_mut();
    };

    // Underlying realloc. Contract: on failure, `old_raw` is still
    // live and its header is still valid (peek was non-destructive).
    let new_raw = real(old_raw, raw_size);
    if new_raw.is_null() {
        return std::ptr::null_mut();
    }

    // On success, libc copied the old block's bytes into `new_raw`
    // starting at offset 0, so the old user data now sits at
    // `new_raw + old_offset`. Shift it down to `new_raw` so the caller
    // sees the user pointer at offset 0 of an unsampled block.
    // memmove (not memcpy) because when the underlying realloc extends
    // in place, `new_raw == old_raw` and source/destination overlap.
    #[cfg(target_arch = "x86_64")]
    {
        let copied_user = (new_raw as *mut u8).add(old_offset).cast::<c_void>();
        if new_raw != copied_user {
            libc::memmove(new_raw, copied_user, size);
        }
    }

    // Fire ddheap:free for the OLD user pointer (the address the
    // profiler last saw as live). We call `dd_probe_free` directly,
    // not `dd_allocation_freed`, because there's no header left to
    // clear: libc has already consumed the old block and we've
    // overwritten those bytes above.
    dd_probe_free(ptr);
    new_raw
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
    let guard = GotterReentry::enter();
    if guard.reentered() {
        return real(memptr, alignment, size);
    }
    ensure_tls();
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
    let guard = GotterReentry::enter();
    if guard.reentered() {
        return real(alignment, size);
    }
    ensure_tls();
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
    // New library may have introduced new GOT entries that need patching.
    crate::update_heap_overrides();
    handle
}

/// Args we package up so the wrapped start routine sees its original
/// arg through our trampoline. Matches ddprof's `Args = tuple<...>`.
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
