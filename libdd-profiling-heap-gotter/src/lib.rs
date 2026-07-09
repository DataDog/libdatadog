// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition for heap profiling.
//!
//! This crate installs hook functions over a running process's dynamic
//! symbol relocations (the GOT / PLT-resolved entries) so that calls
//! such as `malloc` and `free` are routed through
//! [`libdd-profiling-heap-sampler`] without recompiling or relinking the target
//! application. The approach mirrors ddprof's `src/lib/symbol_overrides.cc`
//! + `src/lib/elfutils.cc`.
//!
//! The public API is available on every platform so downstream code
//! never has to `#[cfg]`-guard its callers. The GOT-patching machinery
//! itself only exists on 64-bit Linux (where `dl_iterate_phdr` + ELF64
//! relocs are well-defined); on every other target the entry points compile
//! to no-ops and `heap_overrides_are_installed()` always returns
//! `false`.
//!
//! # Quickstart
//!
//! ```no_run
//! libdd_profiling_heap_gotter::install_heap_overrides();
//! // ... application runs for the rest of its life; malloc/free/calloc/
//! //     realloc/etc. flow through libdd-profiling-heap-sampler and emit
//! //     ddheap:alloc / ddheap:free USDTs ...
//! ```
//!
//! # Installation is permanent (there is no uninstall)
//!
//! There is intentionally no way to remove the overrides once installed:
//! the hooks stay in place for the life of the process. Sampled
//! allocations carry an inline header (x86-64) or pointer tag (arm64)
//! that only our `free`/`realloc` hooks know how to unwrap. If we
//! unpatched those hooks while any tagged allocation were still live, its
//! eventual free would hand an offset/tagged pointer straight to the real
//! allocator and corrupt the heap - and we cannot know when the last
//! tagged allocation has been freed.
//!
//! # Features
//!
//! * `live-heap` (off by default) — enables live-heap tracking: interposed allocations are flagged
//!   and frees are sampled, so a profiler can balance allocs against frees. Off = allocation
//!   profiling only. (Distinct from the runtime `DD_HEAP_SAMPLING_ENABLED` bypass.)
//!
//! # Status
//!
//! Initial port. Covers: `malloc`, `free`, `calloc`, `realloc`,
//! `posix_memalign`, `aligned_alloc`, plus `dlopen` (to re-scan on new
//! library load) and `pthread_create` (to materialise sampler TLS up
//! front on new threads).
//!
//! Not yet covered:
//! * `operator new` / `operator delete` family
//! * `mmap` / `munmap` (sampler-side API doesn't exist yet)
//! * jemalloc-specific `mallocx`/`dallocx`/etc.
//! * `pthread_atfork` child handler to reset state cleanly across `fork()`.

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod elf;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod hooks;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::sync::{Mutex, MutexGuard, TryLockError};

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use elf::SymbolOverrides;

/// Holds the SymbolOverrides registry across calls to `install` /
/// `update`. Guarded globally because GOT patching mutates process-wide
/// state.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
static GLOBAL_OVERRIDES: Mutex<Option<SymbolOverrides>> = Mutex::new(None);

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn lock_global_overrides() -> MutexGuard<'static, Option<SymbolOverrides>> {
    GLOBAL_OVERRIDES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Install GOT overrides for the supported allocator and helper symbols.
/// Safe to call more than once: the registry is rebuilt and re-applied,
/// which also picks up any libraries loaded since the last call.
///
/// Returns `true` if at least one symbol was successfully overridden;
/// `false` if nothing could be resolved. This might happen if the
/// target process has already been statically linked against a custom
/// allocator that doesn't appear in the dynamic symbol table.
///
/// On non-64-bit-Linux targets this is a no-op that always returns
/// `false` — the GOT-patching path it would otherwise execute has no
/// portable equivalent outside ELF64 + `dl_iterate_phdr`.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn install_heap_overrides() -> bool {
    // if sampling is disabled via
    // DD_HEAP_SAMPLING_ENABLED, don't touch the GOT at all. The process
    // keeps calling the real allocator symbols directly, exactly as if
    // this crate had never been installed. Returns false (nothing
    // overridden), consistent with the "couldn't install" return.
    if !libdd_profiling_heap_sampler::heap_sampling_enabled() {
        return false;
    }

    let mut guard = lock_global_overrides();
    if guard.is_none() {
        let mut so = SymbolOverrides::new();
        register_all(&mut so);
        *guard = Some(so);
    }
    let so = guard.as_mut().unwrap();
    so.apply_overrides();
    // Heuristic: at least one ORIG slot resolved.
    any_orig_resolved()
}

/// See the Linux variant above.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn install_heap_overrides() -> bool {
    false
}

/// Re-scan loaded libraries and patch any newly-introduced GOT entries.
/// Called automatically from the `dlopen` hook; user code typically
/// doesn't need to call this directly. No-op on non-Linux targets.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn update_heap_overrides() {
    // `try_lock` so a dlopen happening on the same thread that owns the
    // install lock doesn't deadlock - that thread will finish its
    // outer apply_overrides, which already walks every library.
    let mut guard = match GLOBAL_OVERRIDES.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    if let Some(so) = guard.as_mut() {
        so.update_overrides();
    }
}

/// See the Linux variant above.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn update_heap_overrides() {}

/// Return whether heap GOT overrides are currently installed. Always
/// returns `false` on non-Linux targets, since `install_heap_overrides`
/// is a no-op there.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn heap_overrides_are_installed() -> bool {
    lock_global_overrides().is_some()
}

/// See the Linux variant above.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn heap_overrides_are_installed() -> bool {
    false
}

/// Number of times a `gotter_*` hook has run in this process. Test-only:
/// lets integration tests in other crates (see
/// `libdd-profiling-heap-gotter-ffi/tests/install.rs`) prove the patched
/// GOT was actually exercised, not just that nothing crashed. Always `0`
/// on non-64-bit-Linux targets, where hooks never run.
#[cfg(feature = "test-support")]
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn test_hook_hits() -> u64 {
    hooks::HOOK_HITS.load(std::sync::atomic::Ordering::Relaxed) as u64
}

/// See the Linux variant above.
#[cfg(feature = "test-support")]
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn test_hook_hits() -> u64 {
    0
}

/// Register GOT overrides for every symbol this crate currently hooks.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn register_all(so: &mut SymbolOverrides) {
    use hooks::*;
    use std::sync::atomic::AtomicUsize;

    // Register one entry per supported symbol. The install path stores
    // via `store(Release)` and hooks read via `load(Acquire)`; both go
    // through the typed atomic to avoid racing plain writes against
    // atomic loads.
    fn reg(so: &mut SymbolOverrides, name: &str, hook_addr: usize, slot: &'static AtomicUsize) {
        so.register(name, hook_addr, slot);
    }

    reg(
        so,
        "malloc",
        gotter_malloc as *const () as usize,
        &ORIG_MALLOC,
    );
    reg(so, "free", gotter_free as *const () as usize, &ORIG_FREE);
    reg(
        so,
        "calloc",
        gotter_calloc as *const () as usize,
        &ORIG_CALLOC,
    );
    reg(
        so,
        "realloc",
        gotter_realloc as *const () as usize,
        &ORIG_REALLOC,
    );
    reg(
        so,
        "posix_memalign",
        gotter_posix_memalign as *const () as usize,
        &ORIG_POSIX_MEMALIGN,
    );
    reg(
        so,
        "aligned_alloc",
        gotter_aligned_alloc as *const () as usize,
        &ORIG_ALIGNED_ALLOC,
    );
    reg(
        so,
        "dlopen",
        gotter_dlopen as *const () as usize,
        &ORIG_DLOPEN,
    );
    reg(
        so,
        "pthread_create",
        gotter_pthread_create as *const () as usize,
        &ORIG_PTHREAD_CREATE,
    );
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn any_orig_resolved() -> bool {
    use hooks::*;
    use std::sync::atomic::Ordering;
    [
        &ORIG_MALLOC,
        &ORIG_FREE,
        &ORIG_CALLOC,
        &ORIG_REALLOC,
        &ORIG_POSIX_MEMALIGN,
        &ORIG_ALIGNED_ALLOC,
        &ORIG_DLOPEN,
        &ORIG_PTHREAD_CREATE,
    ]
    .iter()
    .any(|s| s.load(Ordering::Relaxed) != 0)
}

// Tests call into the ELF symbol-lookup path (dl_iterate_phdr +
// dynsym parsing of loaded libraries) which miri can't execute, so
// skip the whole module under miri.
#[cfg(all(test, target_os = "linux", target_pointer_width = "64", not(miri)))]
mod tests {
    use super::*;

    /// Smoke test that doesn't actually install (avoids messing with
    /// the test binary's allocator) but exercises the symbol-lookup
    /// path. `malloc` is always present in a Linux process.
    #[test]
    fn can_lookup_malloc() {
        let r = elf::lookup_symbol("malloc", 0);
        assert!(r.is_some(), "expected to find malloc in loaded libraries");
        let r = r.unwrap();
        assert!(r.address != 0);
    }

    #[test]
    fn unknown_symbol_lookup_returns_none() {
        let r = elf::lookup_symbol("definitely_not_a_real_libc_symbol_xyzzy", 0);
        assert!(r.is_none());
    }
}
