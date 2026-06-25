// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition for heap profiling.
//!
//! This crate installs hook functions over a running process's dynamic
//! symbol relocations (the GOT / PLT-resolved entries) so that calls
//! such as `malloc` and `free` are routed through
//! [`libdd-heap-sampler`] without recompiling or relinking the target
//! application. The approach mirrors ddprof's `src/lib/symbol_overrides.cc`
//! + `src/lib/elfutils.cc`.
//!
//! The public API is available on every platform so downstream code
//! never has to `#[cfg]`-guard its callers. The GOT-patching machinery
//! itself only exists on Linux (where `dl_iterate_phdr` + ELF relocs
//! are well-defined); on every other target the entry points compile
//! to no-ops and `heap_overrides_are_installed()` always returns
//! `false`.
//!
//! # Quickstart
//!
//! ```no_run
//! libdd_heap_gotter::install_heap_overrides();
//! // ... application runs; malloc/free/calloc/realloc/etc. flow through
//! //     libdd-heap-sampler and emit ddheap:alloc / ddheap:free USDTs ...
//! libdd_heap_gotter::restore_heap_overrides();
//! ```
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

#[cfg(target_os = "linux")]
mod elf;
#[cfg(target_os = "linux")]
mod hooks;

#[cfg(target_os = "linux")]
use std::sync::Mutex;

#[cfg(target_os = "linux")]
use elf::SymbolOverrides;

/// Holds the SymbolOverrides registry across calls to `install` / `update`
/// / `restore`. ddprof keeps the equivalent state in
/// `g_symbol_overrides` guarded by `g_mutex`
#[cfg(target_os = "linux")]
static GLOBAL_OVERRIDES: Mutex<Option<SymbolOverrides>> = Mutex::new(None);

/// Install GOT overrides for the supported allocator and helper symbols.
/// Safe to call more than once: the registry is rebuilt and re-applied,
/// which also picks up any libraries loaded since the last call.
///
/// Returns `true` if at least one symbol was successfully overridden;
/// `false` if nothing could be resolved. This might happen if the
/// target process has already been statically linked against a custom
/// allocator that doesn't appear in the dynamic symbol table.
///
/// On non-Linux targets this is a no-op that always returns `false` —
/// the GOT-patching path it would otherwise execute has no portable
/// equivalent outside ELF + `dl_iterate_phdr`.
#[cfg(target_os = "linux")]
pub fn install_heap_overrides() -> bool {
    let mut guard = GLOBAL_OVERRIDES.lock().expect("gotter mutex poisoned");
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
#[cfg(not(target_os = "linux"))]
pub fn install_heap_overrides() -> bool {
    false
}

/// Re-scan loaded libraries and patch any newly-introduced GOT entries.
/// Called automatically from the `dlopen` hook; user code typically
/// doesn't need to call this directly. No-op on non-Linux targets.
#[cfg(target_os = "linux")]
pub fn update_heap_overrides() {
    // `try_lock` so a dlopen happening on the same thread that owns the
    // install lock doesn't deadlock - that thread will finish its
    // outer apply_overrides, which already walks every library.
    if let Ok(mut guard) = GLOBAL_OVERRIDES.try_lock() {
        if let Some(so) = guard.as_mut() {
            so.update_overrides();
        }
    }
}

/// See the Linux variant above.
#[cfg(not(target_os = "linux"))]
pub fn update_heap_overrides() {}

/// Revert every GOT entry we patched. After this call, the process is
/// once again calling the real allocator symbols directly. No-op on
/// non-Linux targets.
#[cfg(target_os = "linux")]
pub fn restore_heap_overrides() {
    let mut guard = GLOBAL_OVERRIDES.lock().expect("gotter mutex poisoned");
    if let Some(so) = guard.as_mut() {
        so.restore_overrides();
    }
    *guard = None;
}

/// See the Linux variant above.
#[cfg(not(target_os = "linux"))]
pub fn restore_heap_overrides() {}

/// Return whether heap GOT overrides are currently installed. Always
/// returns `false` on non-Linux targets, since `install_heap_overrides`
/// is a no-op there.
#[cfg(target_os = "linux")]
pub fn heap_overrides_are_installed() -> bool {
    GLOBAL_OVERRIDES
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// See the Linux variant above.
#[cfg(not(target_os = "linux"))]
pub fn heap_overrides_are_installed() -> bool {
    false
}

/// Register GOT overrides for every symbol this crate currently hooks.
#[cfg(target_os = "linux")]
fn register_all(so: &mut SymbolOverrides) {
    use hooks::*;
    use std::sync::atomic::AtomicUsize;

    // Register one entry per supported symbol. The `ref_slot` raw
    // pointer is to a `'static AtomicUsize`, so it's valid forever.
    // AtomicUsize is repr(transparent) over UnsafeCell<usize>; we
    // intentionally bypass its API for the install-time write because
    // we hand the raw `*mut usize` to the ELF GOT scanner. Hooks then
    // read it back via `Atomic::load(Acquire)`.
    fn reg(so: &mut SymbolOverrides, name: &str, hook_addr: usize, slot: &'static AtomicUsize) {
        let slot_ptr = slot as *const AtomicUsize as *mut usize;
        so.register(name, hook_addr, slot_ptr);
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

#[cfg(target_os = "linux")]
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
#[cfg(all(test, target_os = "linux", not(miri)))]
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
        assert!(r.size > 0);
    }

    #[test]
    fn unknown_symbol_lookup_returns_none() {
        let r = elf::lookup_symbol("definitely_not_a_real_libc_symbol_xyzzy", 0);
        assert!(r.is_none());
    }
}
