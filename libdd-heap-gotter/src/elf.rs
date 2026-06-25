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
//! * GNU hash tables only - `DT_HASH` is skipped, matching ddprof (sometimes points into kernel
//!   space on old glibcs).
//! * REL / RELA / JMPREL relocation arrays.

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::ffi::CStr;

use libc::{
    dl_iterate_phdr, dl_phdr_info, mprotect, sysconf, Elf64_Phdr, Elf64_Rel, Elf64_Rela, Elf64_Sym,
    PROT_READ, PROT_WRITE, PT_DYNAMIC, PT_LOAD, _SC_PAGESIZE,
};

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
    base_address: usize,
}

impl DynamicInfo {
    /// Read DT_* entries out of a PT_DYNAMIC array. ddprof's
    /// `retrieve_dynamic_info` handles the glibc-vs-musl quirk where
    /// glibc stores absolute addresses in DT entries while musl stores
    /// load-relative offsets; we use the same `addr > base ? addr : base + addr`
    /// heuristic.
    unsafe fn from_phdr(info: &dl_phdr_info) -> Option<Self> {
        let phdrs = std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
        let dyn_phdr = phdrs.iter().find(|p| p.p_type == PT_DYNAMIC)?;
        let dyn_begin = (info.dlpi_addr as usize + dyn_phdr.p_vaddr as usize) as *const Elf64_Dyn;
        let base = info.dlpi_addr as usize;
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

        let sym_count = gnu_hash_symbol_count(gnu_hash);

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

unsafe fn gnu_hash_symbol_count(hashtab: *const u32) -> u32 {
    let nbuckets = *hashtab;
    let symbias = *hashtab.add(1);
    let bloom_size = *hashtab.add(2);
    // 4 header words + bloom (one Elf64_Addr per entry == 2 u32s)
    let mut p = hashtab.add(4 + 2 * bloom_size as usize);
    let buckets = std::slice::from_raw_parts(p, nbuckets as usize);
    p = p.add(nbuckets as usize);
    let chain_zero = p.offset(-(symbias as isize));

    if nbuckets == 0 {
        return 0;
    }
    let mut idx = *buckets.iter().max().unwrap();
    while *chain_zero.add(idx as usize) & 1 == 0 {
        idx += 1;
    }
    idx + 1
}

unsafe fn gnu_hash_lookup(info: &DynamicInfo, name: &[u8]) -> Option<Elf64_Sym> {
    let hashtab = info.gnu_hash;
    let nbuckets = *hashtab;
    let symbias = *hashtab.add(1);
    let bloom_size = *hashtab.add(2);
    let bloom_shift = *hashtab.add(3);
    let bloom = hashtab.add(4) as *const u64;
    let mut p = hashtab.add(4 + 2 * bloom_size as usize);
    let buckets = p;
    p = p.add(nbuckets as usize);
    let chain_zero = p.offset(-(symbias as isize));

    if nbuckets == 0 {
        return None;
    }

    let h = gnu_hash(name);
    let word = *bloom.add(((h / 64) & (bloom_size - 1)) as usize);
    let bit1 = h & 63;
    let bit2 = (h >> bloom_shift) & 63;
    if ((word >> bit1) & (word >> bit2) & 1) == 0 {
        return None;
    }

    let mut symidx = *buckets.add((h % nbuckets) as usize);
    if symidx == STN_UNDEF {
        return None;
    }

    loop {
        let chain_h = *chain_zero.add(symidx as usize);
        if ((chain_h ^ h) >> 1) == 0 {
            if let Some(sname) = info.sym_name(symidx) {
                if sname.to_bytes() == name && check_sym(&*info.symtab.add(symidx as usize)) {
                    return Some(*info.symtab.add(symidx as usize));
                }
            }
        }
        if chain_h & 1 != 0 {
            break;
        }
        symidx += 1;
    }
    None
}

/// Mirror of ddprof's `check`: defining symbol, function/object/notype.
fn check_sym(sym: &Elf64_Sym) -> bool {
    const SHN_ABS: u16 = 0xfff1;
    let stt = sym.st_info & 0xf;
    if sym.st_value == 0 && sym.st_shndx != SHN_ABS {
        return false;
    }
    // STT_NOTYPE(0), STT_OBJECT(1), STT_FUNC(2), STT_GNU_IFUNC(10)
    matches!(stt, 0 | 1 | 2 | 10)
}

/// Visit each loaded ELF object once. `is_exe` is true only on the
/// first callback (the main executable). The callback returns `true` to
/// stop iteration.
fn iterate_libraries<F: FnMut(&dl_phdr_info, bool) -> bool>(mut cb: F) {
    struct Ctx<'a> {
        cb: &'a mut dyn FnMut(&dl_phdr_info, bool) -> bool,
        is_first: bool,
    }
    let mut ctx = Ctx {
        cb: &mut cb,
        is_first: true,
    };

    unsafe extern "C" fn trampoline(
        info: *mut dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        let ctx = &mut *(data as *mut Ctx);
        let is_exe = ctx.is_first;
        ctx.is_first = false;
        if (ctx.cb)(&*info, is_exe) {
            1
        } else {
            0
        }
    }

    unsafe {
        dl_iterate_phdr(Some(trampoline), &mut ctx as *mut _ as *mut c_void);
    }
}

/// Temporarily make the containing page writable and replace one GOT entry.
unsafe fn override_entry(addr: usize, new_value: usize) -> bool {
    let page = sysconf(_SC_PAGESIZE) as usize;
    let aligned = (addr & !(page - 1)) as *mut c_void;
    if mprotect(aligned, page, PROT_READ | PROT_WRITE) != 0 {
        return false;
    }
    std::ptr::write_unaligned(addr as *mut usize, new_value);
    true
}

/// Read one GOT entry without assuming pointer alignment.
unsafe fn read_entry(addr: usize) -> usize {
    std::ptr::read_unaligned(addr as *const usize)
}

#[derive(Clone, Copy)]
pub struct LookupResult {
    pub address: usize,
    #[allow(dead_code)] // exposed for diagnostics / future filtering
    pub size: u64,
}

/// Look up a symbol across loaded objects, returning the first
/// non-zero-sized definition whose address is not `not_this_symbol`.
/// Mirrors ddprof's `lookup_symbol(..., accept_null_sized_symbol=false)`.
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
                    found = Some(LookupResult {
                        address: addr,
                        size: sym.st_size,
                    });
                    return true; // stop
                }
            }
        }
        false
    });
    found
}

/// Per-library revert info: GOT addr -> original value at that addr.
#[derive(Default)]
struct LibraryRevertInfo {
    /// Kept for diagnostics / future logging; we identify libraries by
    /// base address elsewhere.
    #[allow(dead_code)]
    library_name: String,
    old_value_per_address: HashMap<usize, usize>,
    processed: bool,
}

/// One registered override entry.
struct OverrideInfo {
    /// Output slot the install path fills with the resolved real symbol
    /// address (so hooks can call through it). `AtomicUsize` is what the
    /// hook side sees; we just store its address here.
    ref_slot: *mut usize,
    /// Address of our hook function, written into matching GOT entries.
    new_symbol: usize,
    /// If a GOT entry's address equals this, leave it alone. Used to
    /// avoid clobbering our own ref slot's relocation in this library
    /// (otherwise applying our override would replace the resolved real
    /// symbol with our hook, causing infinite recursion when the hook
    /// calls back through `ref_slot`).
    do_not_override_this_symbol: usize,
}

unsafe impl Send for OverrideInfo {}
unsafe impl Sync for OverrideInfo {}

/// Mirror of ddprof's `SymbolOverrides`. Holds the override table and
/// the per-library revert info needed to undo writes.
#[derive(Default)]
pub struct SymbolOverrides {
    overrides: HashMap<String, OverrideInfo>,
    revert_info_per_library: HashMap<usize, LibraryRevertInfo>,
    last_seen_nb_libs: i32,
}

impl SymbolOverrides {
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
            revert_info_per_library: HashMap::new(),
            last_seen_nb_libs: -1,
        }
    }

    /// Register an override. `ref_slot` is filled in by `apply_overrides`
    /// with the resolved address of the real symbol so the hook can call
    /// through it.
    pub fn register(&mut self, name: &str, hook: usize, ref_slot: *mut usize) {
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
                unsafe { *ov.ref_slot = addr };
            }
        }
        self.update_overrides();
    }

    /// Process any newly-loaded libraries (e.g. after `dlopen`).
    /// No-op if the loaded-library count hasn't changed.
    pub fn update_overrides(&mut self) {
        // `dl_phdr_info::dlpi_adds` is incremented on every dlopen.
        // ddprof uses it as a cheap "did anything change?" probe.
        let mut nb_loaded: i32 = -1;
        iterate_libraries(|info, _| {
            nb_loaded = info.dlpi_adds as i32;
            true
        });
        if nb_loaded == self.last_seen_nb_libs {
            return;
        }
        self.last_seen_nb_libs = nb_loaded;

        for v in self.revert_info_per_library.values_mut() {
            v.processed = false;
        }

        // SAFETY: closure runs synchronously inside dl_iterate_phdr.
        let self_ptr = self as *mut Self as usize;
        iterate_libraries(move |info, _is_exe| unsafe {
            let this = &mut *(self_ptr as *mut Self);
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
                this.apply_to_library(&dyn_info, lib_name);
            }
            false
        });

        // Drop any tracked libraries that have been unloaded.
        self.revert_info_per_library.retain(|_, v| v.processed);
    }

    /// Restore every GOT entry we touched.
    pub fn restore_overrides(&mut self) {
        let info_per_lib = std::mem::take(&mut self.revert_info_per_library);
        for (_base, revert) in info_per_lib {
            unsafe {
                for (addr, old) in revert.old_value_per_address {
                    override_entry(addr, old);
                }
            }
        }
        self.last_seen_nb_libs = -1;
    }

    unsafe fn apply_to_library(&mut self, dyn_info: &DynamicInfo, library_name: String) {
        let (entry_is_new, _) = match self.revert_info_per_library.entry(dyn_info.base_address) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(LibraryRevertInfo {
                    library_name: library_name.clone(),
                    processed: true,
                    ..Default::default()
                });
                (true, ())
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().processed = true;
                (false, ())
            }
        };
        if !entry_is_new {
            return;
        }

        // Hand-managed split borrow so we can pass &overrides + &mut revert.
        let revert = self
            .revert_info_per_library
            .get_mut(&dyn_info.base_address)
            .unwrap();

        if !dyn_info.rels.is_null() {
            let relocs = std::slice::from_raw_parts(dyn_info.rels, dyn_info.rels_count);
            for reloc in relocs {
                Self::process_relocation(
                    &self.overrides,
                    dyn_info,
                    elf64_r_sym(reloc.r_info) as u32,
                    reloc.r_offset as usize,
                    revert,
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
                    revert,
                );
            }
        }
    }

    unsafe fn process_relocation(
        overrides: &HashMap<String, OverrideInfo>,
        dyn_info: &DynamicInfo,
        sym_index: u32,
        r_offset: usize,
        revert: &mut LibraryRevertInfo,
    ) {
        // st_name -> string in strtab. Walk lazily: we look up the
        // name in the override map; if it's not there, skip.
        let sym = &*dyn_info.symtab.add(sym_index as usize);
        let name_off = sym.st_name as usize;
        if name_off == 0 || name_off >= dyn_info.strtab_size {
            return;
        }
        let cstr = CStr::from_ptr(dyn_info.strtab.add(name_off));
        let Ok(name) = cstr.to_str() else { return };

        let Some(ov) = overrides.get(name) else {
            return;
        };
        // `ref_slot==0` means we never resolved the real symbol, so the
        // hook would call a NULL pointer. Skip.
        let real = unsafe { *ov.ref_slot };
        if real == 0 {
            return;
        }

        let addr = r_offset + dyn_info.base_address;
        if addr == ov.do_not_override_this_symbol {
            return;
        }
        if revert.old_value_per_address.contains_key(&addr) {
            return;
        }
        revert.old_value_per_address.insert(addr, read_entry(addr));
        override_entry(addr, ov.new_symbol);
    }
}

fn elf64_r_sym(info: u64) -> u64 {
    info >> 32
}

// Phdr/PT_LOAD ranges are unused for now; kept for future "skip self
// library" logic. Silence unused warnings.
#[allow(dead_code)]
const _: () = {
    let _ = PT_LOAD;
};

#[allow(dead_code)]
fn _phdr_marker(_: Elf64_Phdr) {}
