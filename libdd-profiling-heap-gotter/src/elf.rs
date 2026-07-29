// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition for heap profiling.
//!
//! Uses the shared ELF primitives from `libdd-got-hook` for parsing and
//! patching, and adds the multi-symbol `SymbolOverrides` registry with
//! per-library dedup and dlopen rescan support on top.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use libdd_gotter::lookup_symbol;
use libdd_gotter::{
    elf64_r_sym, elf64_r_type, is_got_pointer_reloc, iterate_libraries, DynamicInfo, PageProtGuard,
};

/// Per-library bookkeeping for the GOT re-scan. We never un-patch (see
/// the crate docs on why un-installing can't be done safely), so this
/// records nothing about how to revert - only enough to avoid
/// re-processing a library on later `dlopen`-triggered rescans and to
/// detect base-address reuse.
#[derive(Default)]
struct PatchedLibrary {
    /// Identifies the library at this base address, so we can detect
    /// base-address reuse after a `dlclose` + `dlopen` places a
    /// different library at the same load address.
    ///
    /// Known limitation: detection keys on this name, not on library
    /// contents. A different version of the same library reloaded at the
    /// same base (identical path, changed contents, same base despite
    /// ASLR) would slip through and we would restore stale GOT values.
    /// This is judged unlikely enough in practice to document rather than
    /// guard against with additional fingerprinting.
    library_name: String,
    /// Set each pass in which this library was seen; used to drop entries
    /// for libraries that have since been unloaded.
    processed: bool,
}

/// One registered override entry.
struct OverrideInfo {
    /// Output slot the install path fills with the resolved real symbol
    /// address (so hooks can call through it). This is a shared static
    /// atomic supplied by the caller; the install-time write goes through
    /// `store(Release)` to pair with the hook-side `load(Acquire)`.
    ref_slot: &'static AtomicUsize,
    /// Address of our hook function, written into matching GOT entries.
    new_symbol: usize,
    /// If a GOT entry's address equals this, leave it alone. Used to
    /// avoid clobbering our own ref slot's relocation in this library
    /// (otherwise applying our override would replace the resolved real
    /// symbol with our hook, causing infinite recursion when the hook
    /// calls back through `ref_slot`).
    do_not_override_this_symbol: usize,
}

/// Holds the override table and per-library bookkeeping for GOT rescans.
pub struct SymbolOverrides {
    overrides: HashMap<String, OverrideInfo>,
    patched_libraries: HashMap<usize, PatchedLibrary>,
    last_seen_nb_libs: i32,
}

impl Default for SymbolOverrides {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolOverrides {
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
            patched_libraries: HashMap::new(),
            // -1 is the "never scanned" sentinel; a derived Default would
            // use 0 (a valid library count) and could wrongly skip the
            // first update_overrides, so Default must go through new().
            last_seen_nb_libs: -1,
        }
    }

    /// Register an override. `ref_slot` is filled in by `apply_overrides`
    /// with the resolved address of the real symbol so the hook can call
    /// through it. The install path publishes via `store(Release)`.
    pub fn register(&mut self, name: &str, hook: usize, ref_slot: &'static AtomicUsize) {
        self.overrides.insert(
            name.to_string(),
            OverrideInfo {
                ref_slot,
                new_symbol: hook,
                // Filled in by apply_overrides: we set it to the address
                // of our own `ref_slot` once we know it. For a static
                // Rust we can pass 0 - see note in apply_overrides.
                do_not_override_this_symbol: 0,
            },
        );
    }

    /// Resolve real-symbol addresses, then walk every loaded library and
    /// patch GOT entries.
    pub fn apply_overrides(&mut self) {
        // 1. Resolve each override's underlying real symbol so hooks can forward through it.
        //    Excluding our own hook function address avoids picking up a self-reference (when the
        //    gotter library itself exports the same name - it won't in our case, but cheap
        //    insurance).
        let resolved: Vec<(String, usize)> = self
            .overrides
            .iter()
            .filter_map(|(name, ov)| {
                lookup_symbol(name, ov.new_symbol).map(|r| (name.clone(), r.address))
            })
            .collect();
        for (name, addr) in resolved {
            if let Some(ov) = self.overrides.get_mut(&name) {
                // Release pairs with the hook-side Acquire load.
                ov.ref_slot.store(addr, Ordering::Release);
            }
        }
        self.update_overrides();
    }

    /// Process any newly-loaded libraries (e.g. after `dlopen`).
    /// No-op if the loaded-library count hasn't changed.
    pub fn update_overrides(&mut self) {
        // `dl_phdr_info::dlpi_adds` is incremented on every dlopen.
        // Use it as a cheap "did anything change?" probe.
        let mut nb_loaded: i32 = -1;
        iterate_libraries(|info, _| {
            nb_loaded = info.dlpi_adds as i32;
            true
        });
        if nb_loaded == self.last_seen_nb_libs {
            return;
        }
        self.last_seen_nb_libs = nb_loaded;

        for v in self.patched_libraries.values_mut() {
            v.processed = false;
        }

        // TODO: This is intentionally simple but expensive on workloads that
        // dlopen many libraries: every change re-walks all loaded objects,
        // re-parses their dynamic sections/GNU hash tables, and eagerly reads
        // /proc/self/maps via PageProtGuard even if only one new object needs
        // patching. Track already-processed libraries and lazily create the
        // page-protection guard to avoid repeated heavy work.

        let mut guard = PageProtGuard::new();

        // SAFETY: closure runs synchronously inside dl_iterate_phdr.
        let self_ptr = self as *mut Self as usize;
        let guard_ptr = &mut guard as *mut PageProtGuard as usize;
        iterate_libraries(move |info, _is_exe| unsafe {
            let this = &mut *(self_ptr as *mut Self);
            let g = &mut *(guard_ptr as *mut PageProtGuard);
            let lib_name = if info.dlpi_name.is_null() {
                String::new()
            } else {
                CStr::from_ptr(info.dlpi_name)
                    .to_string_lossy()
                    .into_owned()
            };
            if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
                return false;
            }
            if let Some(dyn_info) = DynamicInfo::from_phdr(info) {
                this.apply_to_library(&dyn_info, lib_name, g);
            }
            false
        });

        // `guard` restores page protections when it drops at end of scope.

        // Drop any tracked libraries that have been unloaded.
        self.patched_libraries.retain(|_, v| v.processed);
    }

    /// Patch every override-matching GOT entry in one loaded library. Skips
    /// libraries already processed this pass and handles base-address reuse
    /// (a `dlclose` + `dlopen` placing a different library at the same base).
    ///
    /// # Safety
    ///
    /// `dyn_info` must have been produced by [`DynamicInfo::from_phdr`] for a
    /// library that is currently loaded, so its symtab/strtab/relocation
    /// pointers are valid and the object is still mapped at
    /// `dyn_info.base_address`. Call only from inside [`iterate_libraries`],
    /// while `dl_iterate_phdr` holds the loader lock.
    unsafe fn apply_to_library(
        &mut self,
        dyn_info: &DynamicInfo,
        library_name: String,
        guard: &mut PageProtGuard,
    ) {
        // Detect base-address reuse: a previous `dlclose` may have freed
        // the load address, and a later `dlopen` can place a different
        // library at the same address. If the name differs from what we
        // recorded, treat this as a fresh library so its GOT gets patched.
        let entry_is_new = match self.patched_libraries.entry(dyn_info.base_address()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(PatchedLibrary {
                    library_name,
                    processed: true,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut e)
                if e.get().library_name != library_name =>
            {
                // Base-address reuse: replace the stale entry.
                e.insert(PatchedLibrary {
                    library_name,
                    processed: true,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().processed = true;
                false
            }
        };
        if !entry_is_new {
            return;
        }

        for reloc in dyn_info.rels() {
            Self::process_relocation(
                &self.overrides,
                dyn_info,
                elf64_r_sym(reloc.r_info) as u32,
                reloc.r_offset as usize,
                guard,
            );
        }
        for relocs in [dyn_info.relas(), dyn_info.jmprels()] {
            for reloc in relocs {
                if !is_got_pointer_reloc(elf64_r_type(reloc.r_info)) {
                    continue;
                }
                Self::process_relocation(
                    &self.overrides,
                    dyn_info,
                    elf64_r_sym(reloc.r_info) as u32,
                    reloc.r_offset as usize,
                    guard,
                );
            }
        }
    }

    /// Resolve one relocation's symbol name and, if it matches a registered
    /// override, rewrite the GOT entry at `r_offset` to point at the hook.
    ///
    /// # Safety
    ///
    /// `dyn_info` must be valid for a currently-loaded object (see
    /// [`Self::apply_to_library`]); `sym_index` and `r_offset` must come from
    /// that object's own relocation table; and `guard` must belong to the
    /// current patching pass. Dereferences `dyn_info`'s symtab/strtab and
    /// writes process memory through `guard`.
    unsafe fn process_relocation(
        overrides: &HashMap<String, OverrideInfo>,
        dyn_info: &DynamicInfo,
        sym_index: u32,
        r_offset: usize,
        guard: &mut PageProtGuard,
    ) {
        // st_name -> string in strtab. Walk lazily: we look up the
        // name in the override map; if it's not there, skip. Relocation
        // symbol indices come from the object being inspected, so guard
        // them before dereferencing dyn_info.symtab.
        let Some(cstr) = dyn_info.sym_name(sym_index) else {
            return;
        };
        if cstr.to_bytes().is_empty() {
            return;
        }
        let Ok(name) = cstr.to_str() else { return };

        let Some(ov) = overrides.get(name) else {
            return;
        };
        // `ref_slot==0` means we never resolved the real symbol, so the
        // hook would call a NULL pointer. Skip.
        let real = ov.ref_slot.load(Ordering::Acquire);
        if real == 0 {
            return;
        }

        let addr = r_offset + dyn_info.base_address();
        if addr == ov.do_not_override_this_symbol {
            return;
        }
        // Re-patching an already-hooked entry with the same hook address is
        // idempotent, so no per-entry dedup is needed.
        guard.override_entry(addr, ov.new_symbol);
    }
}
