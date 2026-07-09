// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition primitives.
//!
//! Port of ddprof's `src/lib/elfutils.cc` + the parts of
//! `symbol_overrides.cc` that drive it. Walks each loaded ELF object via
//! `dl_iterate_phdr`, parses its `PT_DYNAMIC` for the symbol/string/hash
//! tables and the relocation arrays, and rewrites GOT entries whose
//! symbol name is in the override map. Records the previous values so
//! the overrides can be reverted.
//!
//! Scope:
//! * 64-bit ELF only (`Elf64_*`). Other targets are gated out at compile time via `#[cfg]` on the
//!   parent module.
//! * GNU hash tables only - `DT_HASH` is skipped because it has caused problems on older glibc
//!   systems.
//! * REL / RELA / JMPREL relocation arrays.

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::ffi::CStr;

use libc::{
    dl_iterate_phdr, dl_phdr_info, mprotect, sysconf, Elf64_Rel, Elf64_Rela, Elf64_Sym,
    _SC_PAGESIZE, PROT_EXEC, PROT_READ, PROT_WRITE, PT_DYNAMIC, PT_LOAD,
};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicUsize, Ordering};

// ELF dynamic-section tags and friends. The `libc` crate doesn't export
// these (they're processor-independent ELF spec constants), so we name
// them locally. Values come from `<elf.h>`.
#[allow(non_camel_case_types)]
#[repr(C)]
struct Elf64_Dyn {
    d_tag: i64,
    d_un: u64, // d_val / d_ptr union; we only ever read it as u64
}
const DT_NULL: i64 = 0;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_STRSZ: i64 = 10;
const DT_REL: i64 = 17;
const DT_RELSZ: i64 = 18;
const DT_PLTREL: i64 = 20;
const DT_JMPREL: i64 = 23;
const DT_PLTRELSZ: i64 = 2;
const DT_GNU_HASH: i64 = 0x6fff_fef5;
const STN_UNDEF: u32 = 0;

/// The subset of an ELF object's `PT_DYNAMIC` entries needed to find and rewrite GOT entries.
struct DynamicInfo {
    strtab: *const c_char,
    strtab_size: usize,
    symtab: *const Elf64_Sym,
    sym_count: u32,
    rels: *const Elf64_Rel,
    rels_count: usize,
    relas: *const Elf64_Rela,
    relas_count: usize,
    jmprels: *const Elf64_Rela,
    jmprels_count: usize,
    gnu_hash: *const u32,
    gnu_hash_words: usize,
    base_address: usize,
}

impl DynamicInfo {
    /// Read DT_* entries out of a PT_DYNAMIC array. Handles the
    /// glibc-vs-musl quirk where glibc stores absolute addresses in DT
    /// entries while musl stores load-relative offsets; we use the
    /// `addr > base ? addr : base + addr` heuristic.
    unsafe fn from_phdr(info: &dl_phdr_info) -> Option<Self> {
        let phdrs = std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
        let dyn_phdr = phdrs.iter().find(|p| p.p_type == PT_DYNAMIC)?;
        let dyn_begin = (info.dlpi_addr as usize + dyn_phdr.p_vaddr as usize) as *const Elf64_Dyn;
        let base = info.dlpi_addr as usize;
        let containing_load_segment_end = |addr: usize| -> Option<usize> {
            phdrs.iter().filter(|p| p.p_type == PT_LOAD).find_map(|p| {
                let start = base.checked_add(p.p_vaddr as usize)?;
                let end = start.checked_add(p.p_memsz as usize)?;
                (addr >= start && addr < end).then_some(end)
            })
        };
        let correct = |a: u64| -> usize {
            let a = a as usize;
            if a > base {
                a
            } else {
                base + a
            }
        };

        let mut strtab: *const c_char = std::ptr::null();
        let mut strtab_size: usize = 0;
        let mut symtab: *const Elf64_Sym = std::ptr::null();
        let mut rels: *const Elf64_Rel = std::ptr::null();
        let mut rels_size: usize = 0;
        let mut relas: *const Elf64_Rela = std::ptr::null();
        let mut relas_size: usize = 0;
        let mut jmprels: *const Elf64_Rela = std::ptr::null();
        let mut jmprels_size: usize = 0;
        let mut gnu_hash: *const u32 = std::ptr::null();
        let mut pltrel_type: i64 = 0;

        let mut it = dyn_begin;
        loop {
            let d = &*it;
            if d.d_tag == DT_NULL {
                break;
            }
            let v = d.d_un;
            match d.d_tag {
                DT_STRTAB => strtab = correct(v) as *const c_char,
                DT_STRSZ => strtab_size = v as usize,
                DT_SYMTAB => symtab = correct(v) as *const Elf64_Sym,
                DT_GNU_HASH => gnu_hash = correct(v) as *const u32,
                DT_REL => rels = correct(v) as *const Elf64_Rel,
                DT_RELA => relas = correct(v) as *const Elf64_Rela,
                DT_JMPREL => jmprels = correct(v) as *const Elf64_Rela,
                DT_RELSZ => rels_size = v as usize,
                DT_RELASZ => relas_size = v as usize,
                DT_PLTRELSZ => jmprels_size = v as usize,
                DT_PLTREL => pltrel_type = v as i64,
                _ => {}
            }
            it = it.add(1);
        }

        // JMPREL entries are RELA only if DT_PLTREL says so.
        if pltrel_type != DT_RELA {
            jmprels = std::ptr::null();
            jmprels_size = 0;
        }

        if strtab.is_null() || symtab.is_null() || gnu_hash.is_null() {
            return None;
        }

        let gnu_hash_addr = gnu_hash as usize;
        let gnu_hash_words = match containing_load_segment_end(gnu_hash_addr) {
            Some(end) => match end.checked_sub(gnu_hash_addr) {
                Some(bytes) => bytes / core::mem::size_of::<u32>(),
                None => return None,
            },
            None => return None,
        };
        let sym_count = gnu_hash_symbol_count(gnu_hash, gnu_hash_words).unwrap_or_else(|| {
            // Fallback for degenerate .gnu.hash (e.g. executables with only
            // undefined imports): estimate dynsym entry count from the common
            // .dynsym-before-.dynstr layout. This is a heuristic, not an ELF
            // guarantee. If it underestimates we may skip patching some
            // relocations; valid relocation indexes should still keep an
            // overestimate from faulting on normal loaded objects.
            let symtab_addr = symtab as usize;
            let strtab_addr = strtab as usize;
            if strtab_addr > symtab_addr {
                let bytes = strtab_addr - symtab_addr;
                (bytes / core::mem::size_of::<Elf64_Sym>()) as u32
            } else {
                // Can't estimate; allow any index and rely on strtab
                // bounds checking in sym_name to catch bad accesses.
                u32::MAX
            }
        });

        Some(Self {
            strtab,
            strtab_size,
            symtab,
            sym_count,
            rels,
            rels_count: rels_size / core::mem::size_of::<Elf64_Rel>(),
            relas,
            relas_count: relas_size / core::mem::size_of::<Elf64_Rela>(),
            jmprels,
            jmprels_count: jmprels_size / core::mem::size_of::<Elf64_Rela>(),
            gnu_hash,
            gnu_hash_words,
            base_address: base,
        })
    }

    unsafe fn sym_name(&self, idx: u32) -> Option<&CStr> {
        if (idx as usize) >= self.sym_count as usize {
            return None;
        }
        let sym = &*self.symtab.add(idx as usize);
        let off = sym.st_name as usize;
        if off >= self.strtab_size {
            return None;
        }
        Some(CStr::from_ptr(self.strtab.add(off)))
    }
}

/// Compute the GNU symbol hash used by `DT_GNU_HASH` tables.
/// See <https://flapenguin.me/elf-dt-gnu-hash>.
fn gnu_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 5381;
    for c in name {
        h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(*c as u32);
    }
    h
}

/// Compute the total number of entries in `.dynsym` from the `.gnu.hash` table.
///
/// Returns `None` only when the table is structurally invalid. When the hash
/// is degenerate (all buckets empty, typical for executables that only import
/// symbols), returns `None` to signal that the caller should use a fallback
/// (e.g. estimate from symtab/strtab distance) since the hash table doesn't
/// tell us how many undefined-import entries precede the hashed region.
unsafe fn gnu_hash_symbol_count(hashtab: *const u32, hashtab_words: usize) -> Option<u32> {
    if hashtab_words < 4 {
        return None;
    }

    let nbuckets = *hashtab;
    let symbias = *hashtab.add(1);
    let bloom_size = *hashtab.add(2);
    let bloom_size_words = (bloom_size as usize).checked_mul(2)?;
    let buckets_start = 4usize.checked_add(bloom_size_words)?;
    let chains_start = buckets_start.checked_add(nbuckets as usize)?;

    if bloom_size == 0 || buckets_start > hashtab_words || chains_start > hashtab_words {
        return None;
    }
    if nbuckets == 0 {
        // No buckets at all: can't determine symtab size from the hash.
        return None;
    }

    let buckets = std::slice::from_raw_parts(hashtab.add(buckets_start), nbuckets as usize);
    let mut idx = *buckets.iter().max()?;
    if idx == STN_UNDEF {
        // All buckets empty: hash covers zero defined symbols, but the
        // symtab may still have undefined imports. Signal the caller to
        // use a fallback.
        return None;
    }
    if idx < symbias {
        return None;
    }

    let chain_count = hashtab_words - chains_start;
    loop {
        let chain_idx = (idx - symbias) as usize;
        if chain_idx >= chain_count {
            return None;
        }
        if *hashtab.add(chains_start + chain_idx) & 1 != 0 {
            return idx.checked_add(1);
        }
        idx = idx.checked_add(1)?;
    }
}

unsafe fn gnu_hash_lookup(info: &DynamicInfo, name: &[u8]) -> Option<Elf64_Sym> {
    let hashtab = info.gnu_hash;
    if info.gnu_hash_words < 4 {
        return None;
    }

    let nbuckets = *hashtab;
    let symbias = *hashtab.add(1);
    let bloom_size = *hashtab.add(2);
    let bloom_shift = *hashtab.add(3);
    let bloom_size_words = (bloom_size as usize).checked_mul(2)?;
    let buckets_start = 4usize.checked_add(bloom_size_words)?;
    let chains_start = buckets_start.checked_add(nbuckets as usize)?;

    if nbuckets == 0
        || bloom_size == 0
        || buckets_start > info.gnu_hash_words
        || chains_start > info.gnu_hash_words
    {
        return None;
    }

    let h = gnu_hash(name);
    let bloom = hashtab.add(4) as *const u64;
    let word = *bloom.add(((h / 64) & (bloom_size - 1)) as usize);
    let bit1 = h & 63;
    let bit2 = (h >> bloom_shift) & 63;
    if ((word >> bit1) & (word >> bit2) & 1) == 0 {
        return None;
    }

    let buckets = hashtab.add(buckets_start);
    let mut symidx = *buckets.add((h % nbuckets) as usize);
    if symidx == STN_UNDEF {
        return None;
    }
    if symidx < symbias {
        return None;
    }

    let chain_count = info.gnu_hash_words - chains_start;
    loop {
        let chain_idx = (symidx - symbias) as usize;
        if chain_idx >= chain_count {
            return None;
        }
        let chain_h = *hashtab.add(chains_start + chain_idx);
        if ((chain_h ^ h) >> 1) == 0 {
            if let Some(sname) = info.sym_name(symidx) {
                let sym = info.symtab.add(symidx as usize);
                if sname.to_bytes() == name && check_sym(&*sym) {
                    return Some(*sym);
                }
            }
        }
        if chain_h & 1 != 0 {
            break;
        }
        symidx = symidx.checked_add(1)?;
    }
    None
}

/// Return whether this is a defining function/object/notype symbol.
fn check_sym(sym: &Elf64_Sym) -> bool {
    const SHN_ABS: u16 = 0xfff1;
    let stt = sym.st_info & 0xf;
    (sym.st_value != 0 || sym.st_shndx == SHN_ABS) &&
        // STT_NOTYPE(0), STT_OBJECT(1), STT_FUNC(2), STT_GNU_IFUNC(10)
        matches!(stt, 0 | 1 | 2 | 10)
}

/// Visit each loaded ELF object once. `is_exe` is true only on the
/// first callback (the main executable). The callback returns `true` to
/// stop iteration.
fn iterate_libraries(mut callback: impl FnMut(&dl_phdr_info, bool) -> bool) {
    struct Ctx<'a> {
        callback: &'a mut dyn FnMut(&dl_phdr_info, bool) -> bool,
        is_first: bool,
    }
    let mut ctx = Ctx {
        callback: &mut callback,
        is_first: true,
    };

    unsafe extern "C" fn trampoline(
        info: *mut dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ctx = &mut *(data as *mut Ctx);
            let is_exe = ctx.is_first;
            ctx.is_first = false;
            (ctx.callback)(&*info, is_exe)
        }));

        // Never unwind a Rust panic through libc's dl_iterate_phdr callback.
        // Treat patching as best-effort and stop iteration on panic.
        result.map(i32::from).unwrap_or(1)
    }

    unsafe {
        dl_iterate_phdr(Some(trampoline), &mut ctx as *mut _ as *mut c_void);
    }
}

/// A single /proc/self/maps entry: address range + current protection flags.
#[derive(Clone, Copy)]
struct MapEntry {
    start: usize,
    end: usize,
    prot: i32,
}

/// Parse /proc/self/maps into a sorted list of (range, prot) entries.
///
/// Used to remember each GOT page's original protection so we can restore
/// it after patching, rather than leaving Full-RELRO pages read-write for
/// the lifetime of the process.
fn read_proc_maps() -> Vec<MapEntry> {
    let Ok(f) = std::fs::File::open("/proc/self/maps") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        let Some(range) = parts.next() else { continue };
        let Some(perms) = parts.next() else { continue };
        let Some(dash) = range.find('-') else {
            continue;
        };
        let Ok(start) = usize::from_str_radix(&range[..dash], 16) else {
            continue;
        };
        let Ok(end) = usize::from_str_radix(&range[dash + 1..], 16) else {
            continue;
        };
        let b = perms.as_bytes();
        let mut prot = 0;
        if b.first() == Some(&b'r') {
            prot |= PROT_READ;
        }
        if b.get(1) == Some(&b'w') {
            prot |= PROT_WRITE;
        }
        if b.get(2) == Some(&b'x') {
            prot |= PROT_EXEC;
        }
        out.push(MapEntry { start, end, prot });
    }
    out
}

/// Batched GOT-entry patcher that remembers each touched page's
/// original protection and restores it at the end of a patching pass.
///
/// On Full-RELRO binaries, GOT pages start read-only. The old
/// `override_entry` helper flipped them to read-write and never
/// restored them, weakening RELRO for the process lifetime. This guard
/// mprotects each unique page once (RW), lets the caller write as
/// many entries as it needs, then mprotects each page back to what
/// `/proc/self/maps` reported at guard-construction time when it is
/// dropped (including on panic or early return).
struct PageProtGuard {
    page_size: usize,
    maps: Vec<MapEntry>,
    // Aligned page base -> original prot flags read from /proc/self/maps.
    touched: HashMap<usize, i32>,
}

impl PageProtGuard {
    fn new() -> Self {
        // sysconf can return -1 on error; fall back to a conservative
        // 4 KiB default if the query fails.
        let raw = unsafe { sysconf(_SC_PAGESIZE) };
        let page_size = usize::try_from(raw).unwrap_or(4096);
        Self {
            page_size,
            maps: read_proc_maps(),
            touched: HashMap::new(),
        }
    }

    fn original_prot(&self, addr: usize) -> Option<i32> {
        self.maps
            .iter()
            .find(|m| addr >= m.start && addr < m.end)
            .map(|m| m.prot)
    }

    /// Make the containing page writable if it isn't already touched,
    /// then replace one GOT entry.
    unsafe fn override_entry(&mut self, addr: usize, new_value: usize) -> bool {
        let aligned = addr & !(self.page_size - 1);
        if !self.touched.contains_key(&aligned) {
            // If /proc/self/maps isn't available (or the page isn't in
            // it, which shouldn't happen for a mapped GOT page) fall
            // back to PROT_READ - the RELRO'd default. That's tighter
            // than the previous behavior of leaving pages RW.
            let orig = self.original_prot(aligned).unwrap_or(PROT_READ);
            if mprotect(
                aligned as *mut c_void,
                self.page_size,
                PROT_READ | PROT_WRITE,
            ) != 0
            {
                return false;
            }
            self.touched.insert(aligned, orig);
        }
        std::ptr::write_unaligned(addr as *mut usize, new_value);
        true
    }
}

impl Drop for PageProtGuard {
    /// Restore every touched page to its original protection. Runs on
    /// scope exit - including panic or early return - so page protections
    /// are never left weakened even if a patching pass bails out midway.
    fn drop(&mut self) {
        for (aligned, orig) in self.touched.drain() {
            // Best-effort: nothing sensible to do on failure other than
            // leave the page RW, which is the pre-fix behavior.
            unsafe { mprotect(aligned as *mut c_void, self.page_size, orig) };
        }
    }
}

#[derive(Clone, Copy)]
pub struct LookupResult {
    pub address: usize,
}

/// Look up a symbol across loaded objects, returning the first
/// non-zero-sized definition whose address is not `not_this_symbol`.
/// Null-sized symbols are ignored so hooks resolve to callable definitions.
pub fn lookup_symbol(name: &str, not_this_symbol: usize) -> Option<LookupResult> {
    let needle = name.as_bytes();
    let mut found: Option<LookupResult> = None;
    iterate_libraries(|info, _is_exe| unsafe {
        let lib_name = if info.dlpi_name.is_null() {
            ""
        } else {
            CStr::from_ptr(info.dlpi_name).to_str().unwrap_or("")
        };
        if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
            return false;
        }
        let Some(dyn_info) = DynamicInfo::from_phdr(info) else {
            return false;
        };
        if let Some(sym) = gnu_hash_lookup(&dyn_info, needle) {
            if sym.st_size > 0 {
                let addr = sym.st_value as usize + dyn_info.base_address;
                if addr != not_this_symbol {
                    found = Some(LookupResult { address: addr });
                    return true; // stop
                }
            }
        }
        false
    });
    found
}

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
        let entry_is_new = match self.patched_libraries.entry(dyn_info.base_address) {
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

        if !dyn_info.rels.is_null() {
            let relocs = std::slice::from_raw_parts(dyn_info.rels, dyn_info.rels_count);
            for reloc in relocs {
                Self::process_relocation(
                    &self.overrides,
                    dyn_info,
                    elf64_r_sym(reloc.r_info) as u32,
                    reloc.r_offset as usize,
                    guard,
                );
            }
        }
        for slice_ptr_and_len in [
            (dyn_info.relas, dyn_info.relas_count),
            (dyn_info.jmprels, dyn_info.jmprels_count),
        ] {
            if slice_ptr_and_len.0.is_null() {
                continue;
            }
            let relocs = std::slice::from_raw_parts(slice_ptr_and_len.0, slice_ptr_and_len.1);
            for reloc in relocs {
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

        let addr = r_offset + dyn_info.base_address;
        if addr == ov.do_not_override_this_symbol {
            return;
        }
        // Re-patching an already-hooked entry with the same hook address is
        // idempotent, so no per-entry dedup is needed.
        guard.override_entry(addr, ov.new_symbol);
    }
}

fn elf64_r_sym(info: u64) -> u64 {
    info >> 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_prot_guard_finds_original_mapping_protection() {
        let guard = PageProtGuard {
            page_size: 4096,
            maps: vec![
                MapEntry {
                    start: 0x1000,
                    end: 0x2000,
                    prot: PROT_READ,
                },
                MapEntry {
                    start: 0x2000,
                    end: 0x3000,
                    prot: PROT_READ | PROT_EXEC,
                },
            ],
            touched: HashMap::new(),
        };

        assert_eq!(guard.original_prot(0x1000), Some(PROT_READ));
        assert_eq!(guard.original_prot(0x1fff), Some(PROT_READ));
        assert_eq!(guard.original_prot(0x2000), Some(PROT_READ | PROT_EXEC));
        assert_eq!(guard.original_prot(0x3000), None);
    }
}
