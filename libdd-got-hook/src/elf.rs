// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition primitives.
//!
//! Scope:
//! * 64-bit Linux ELF only (`Elf64_*`).
//! * GNU hash tables only (`DT_GNU_HASH`). `DT_HASH` is not parsed; objects without a GNU hash
//!   table are skipped.
//! * REL / RELA / JMPREL relocation arrays.

use core::ffi::{c_char, c_int, c_void, CStr};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use libc::{
    dl_iterate_phdr, dl_phdr_info, mprotect, sysconf, Elf64_Rel, Elf64_Rela, Elf64_Sym,
    _SC_PAGESIZE, PROT_EXEC, PROT_READ, PROT_WRITE, PT_DYNAMIC, PT_LOAD,
};

// ELF dynamic-section tags. The `libc` crate doesn't export these
// (they're processor-independent ELF spec constants). Values from `<elf.h>`.
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

/// The subset of an ELF object's `PT_DYNAMIC` entries needed to find
/// and rewrite GOT entries.
pub struct DynamicInfo {
    strtab: *const c_char,
    strtab_size: usize,
    symtab: *const Elf64_Sym,
    sym_count: u32,
    gnu_hash: *const u32,
    gnu_hash_words: usize,
    rels: *const Elf64_Rel,
    rels_count: usize,
    relas: *const Elf64_Rela,
    relas_count: usize,
    jmprels: *const Elf64_Rela,
    jmprels_count: usize,
    base_address: usize,
}

impl DynamicInfo {
    /// Read DT_* entries out of a PT_DYNAMIC array.
    ///
    /// Handles the glibc-vs-musl quirk where glibc stores absolute
    /// addresses in DT entries while musl stores load-relative offsets;
    /// we use the `addr > base ? addr : base + addr` heuristic.
    ///
    /// # Safety
    /// `info` must point to a valid `dl_phdr_info` from `dl_iterate_phdr`.
    pub unsafe fn from_phdr(info: &dl_phdr_info) -> Option<Self> {
        let phdrs = core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
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

        let mut strtab: *const c_char = core::ptr::null();
        let mut strtab_size: usize = 0;
        let mut symtab: *const Elf64_Sym = core::ptr::null();
        let mut rels: *const Elf64_Rel = core::ptr::null();
        let mut rels_size: usize = 0;
        let mut relas: *const Elf64_Rela = core::ptr::null();
        let mut relas_size: usize = 0;
        let mut jmprels: *const Elf64_Rela = core::ptr::null();
        let mut jmprels_size: usize = 0;
        let mut gnu_hash: *const u32 = core::ptr::null();
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
            jmprels = core::ptr::null();
            jmprels_size = 0;
        }

        if strtab.is_null() || symtab.is_null() || gnu_hash.is_null() {
            return None;
        }

        let gnu_hash_addr = gnu_hash as usize;
        let end = containing_load_segment_end(gnu_hash_addr)?;
        let bytes = end.checked_sub(gnu_hash_addr)?;
        let gnu_hash_words = bytes / core::mem::size_of::<u32>();
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
            gnu_hash,
            gnu_hash_words,
            rels,
            rels_count: rels_size / core::mem::size_of::<Elf64_Rel>(),
            relas,
            relas_count: relas_size / core::mem::size_of::<Elf64_Rela>(),
            jmprels,
            jmprels_count: jmprels_size / core::mem::size_of::<Elf64_Rela>(),
            base_address: base,
        })
    }

    /// Look up the name of the symbol at index `idx` in the dynamic
    /// string table.
    ///
    /// # Safety
    /// The `DynamicInfo` must have been produced by [`DynamicInfo::from_phdr`]
    /// for a currently-loaded ELF object whose symtab/strtab are still mapped.
    pub unsafe fn sym_name(&self, idx: u32) -> Option<&CStr> {
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

    /// The base load address of this ELF object.
    pub fn base_address(&self) -> usize {
        self.base_address
    }

    /// Access to REL relocations (pointer, count).
    pub fn rels(&self) -> (*const Elf64_Rel, usize) {
        (self.rels, self.rels_count)
    }

    /// Access to RELA relocations (pointer, count).
    pub fn relas(&self) -> (*const Elf64_Rela, usize) {
        (self.relas, self.relas_count)
    }

    /// Access to JMPREL (PLT) relocations (pointer, count).
    pub fn jmprels(&self) -> (*const Elf64_Rela, usize) {
        (self.jmprels, self.jmprels_count)
    }
}

/// Compute the GNU symbol hash used by `DT_GNU_HASH` tables.
/// See <https://flapenguin.me/elf-dt-gnu-hash>.
pub fn gnu_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 5381;
    for c in name {
        h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(*c as u32);
    }
    h
}

/// Compute the total number of entries in `.dynsym` from the `.gnu.hash`
/// table.
///
/// Returns `None` when the table is structurally invalid or degenerate
/// (all buckets empty).
///
/// # Safety
/// `hashtab` must point to a valid `.gnu.hash` section of at least
/// `hashtab_words` u32 entries in mapped memory.
pub unsafe fn gnu_hash_symbol_count(hashtab: *const u32, hashtab_words: usize) -> Option<u32> {
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
        return None;
    }

    let buckets = core::slice::from_raw_parts(hashtab.add(buckets_start), nbuckets as usize);
    let mut idx = *buckets.iter().max()?;
    if idx == STN_UNDEF {
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

/// Look up a symbol by name in an object's `.gnu.hash` table.
///
/// Returns the `Elf64_Sym` entry if found and valid (non-zero value,
/// function/object/notype binding).
///
/// # Safety
/// `info` must have been produced by [`DynamicInfo::from_phdr`] for a
/// currently-loaded ELF object.
pub unsafe fn gnu_hash_lookup(info: &DynamicInfo, name: &[u8]) -> Option<Elf64_Sym> {
    let hashtab = info.gnu_hash;
    if hashtab.is_null() || info.gnu_hash_words < 4 {
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
pub fn check_sym(sym: &Elf64_Sym) -> bool {
    const SHN_ABS: u16 = 0xfff1;
    let stt = sym.st_info & 0xf;
    (sym.st_value != 0 || sym.st_shndx == SHN_ABS) &&
        // STT_NOTYPE(0), STT_OBJECT(1), STT_FUNC(2), STT_GNU_IFUNC(10)
        matches!(stt, 0 | 1 | 2 | 10)
}

/// Visit each loaded ELF object once. `is_exe` is true only on the
/// first callback (the main executable). The callback returns `true` to
/// stop iteration.
pub fn iterate_libraries(mut callback: impl FnMut(&dl_phdr_info, bool) -> bool) {
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
        result.map(i32::from).unwrap_or(1)
    }

    unsafe {
        dl_iterate_phdr(Some(trampoline), &mut ctx as *mut _ as *mut c_void);
    }
}

/// A single /proc/self/maps entry: address range + current protection flags.
#[derive(Clone, Copy)]
pub struct MapEntry {
    pub start: usize,
    pub end: usize,
    pub prot: i32,
}

/// Parse /proc/self/maps into a list of (range, prot) entries.
///
/// Used to remember each GOT page's original protection so we can restore
/// it after patching, rather than leaving Full-RELRO pages read-write for
/// the lifetime of the process.
pub fn read_proc_maps() -> Vec<MapEntry> {
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
/// On Full-RELRO binaries, GOT pages start read-only. This guard
/// mprotects each unique page once (RW), lets the caller write as
/// many entries as it needs, then mprotects each page back to what
/// `/proc/self/maps` reported at guard-construction time when it is
/// dropped (including on panic or early return).
pub struct PageProtGuard {
    page_size: usize,
    maps: Vec<MapEntry>,
    // Aligned page base -> original prot flags read from /proc/self/maps.
    touched: HashMap<usize, i32>,
}

impl PageProtGuard {
    pub fn new() -> Self {
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

    pub fn original_prot(&self, addr: usize) -> Option<i32> {
        self.maps
            .iter()
            .find(|m| addr >= m.start && addr < m.end)
            .map(|m| m.prot)
    }

    /// Make the containing page writable if it isn't already touched,
    /// then replace one GOT entry.
    ///
    /// # Safety
    /// `addr` must point to a valid GOT slot in mapped memory.
    pub unsafe fn override_entry(&mut self, addr: usize, new_value: usize) -> bool {
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
        core::ptr::write_unaligned(addr as *mut usize, new_value);
        true
    }
}

impl Default for PageProtGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PageProtGuard {
    /// Restore every touched page to its original protection.
    fn drop(&mut self) {
        for (aligned, orig) in self.touched.drain() {
            unsafe { mprotect(aligned as *mut c_void, self.page_size, orig) };
        }
    }
}

/// Extract the symbol index from an ELF64 relocation's `r_info` field.
pub fn elf64_r_sym(info: u64) -> u64 {
    info >> 32
}

/// Result of a symbol lookup.
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
                let addr = sym.st_value as usize + dyn_info.base_address();
                if addr != not_this_symbol {
                    found = Some(LookupResult { address: addr });
                    return true;
                }
            }
        }
        false
    });
    found
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_can_lookup_malloc() {
        let r = lookup_symbol("malloc", 0);
        assert!(r.is_some(), "expected to find malloc in loaded libraries");
        assert!(r.unwrap().address != 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_unknown_symbol_lookup_returns_none() {
        let r = lookup_symbol("definitely_not_a_real_libc_symbol_xyzzy", 0);
        assert!(r.is_none());
    }
}
