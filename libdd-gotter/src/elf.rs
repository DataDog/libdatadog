// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! GOT-table interposition primitives.
//!
//! # Choosing an API
//!
//! * [`hook_symbol`] -- one-shot, single-symbol hook. Only patches libraries that are loaded at
//!   call time. Libraries `dlopen`'d afterwards will **not** have their GOT entries patched, so
//!   calls to the hooked symbol from those libraries will bypass the hook. Use this when you know
//!   the target symbol is already loaded and all relevant callers are already in memory (e.g.
//!   crashtracker hooking `__assert_fail` during `init()`).
//!
//! * [`DynamicInfo`], [`iterate_libraries`], [`PageProtGuard`] -- the lower-level building blocks.
//!   Use these when you need to hook multiple symbols, re-scan after `dlopen` for newly loaded
//!   libraries, or maintain per-library bookkeeping. Example: `libdd-profiling-heap-gotter`'s
//!   `SymbolOverrides` registry, which patches `malloc`/`free`/etc. and re-applies overrides
//!   whenever a new library is loaded.
//!
//! # Scope
//!
//! * 64-bit Linux ELF only (`Elf64_*`).
//! * Supports `DT_GNU_HASH` and falls back to `DT_HASH` (sysv) for determining dynsym entry count.
//! * REL / RELA / JMPREL relocation arrays.

use core::ffi::{c_char, c_int, c_void, CStr};
use core::slice;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

// Lossless integer conversion helpers for 64-bit platforms.
//
// This crate is cfg-gated to `target_pointer_width = "64"`, so
// `u64 -> usize` and `u32 -> usize` are infallible.

/// `u64 -> usize`: lossless on 64-bit platforms.
const fn u64_to_usize(v: u64) -> usize {
    v as usize
}

/// `u32 -> usize`: lossless on 64-bit platforms.
const fn u32_to_usize(v: u32) -> usize {
    v as usize
}

/// Build a slice from a possibly-null pointer and length. Returns `&[]`
/// if the pointer is null or the length is zero, avoiding a call to
/// `slice::from_raw_parts` with invalid arguments.
///
/// # Safety
/// If `ptr` is non-null and `len > 0`, the pointer must be valid for
/// `len * size_of::<T>()` bytes, properly aligned, and not mutated for
/// lifetime `'a`.
unsafe fn try_as_slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        slice::from_raw_parts(ptr, len)
    }
}

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
const DT_HASH: i64 = 4;
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
///
/// Slice fields (`strtab`, `symtab`, `rels`, `relas`, `jmprels`) are
/// validated once in [`from_phdr`] so all subsequent access is safe.
/// The hash table pointers remain raw because the hash lookup functions
/// need arithmetic into their variable-layout internal structure.
pub struct DynamicInfo<'a> {
    strtab: &'a [u8],
    symtab: &'a [Elf64_Sym],
    /// Pointer and word-count for the `.gnu.hash` table, if present.
    /// Used by [`gnu_hash_lookup`] for symbol resolution.
    gnu_hash: *const u32,
    gnu_hash_words: usize,
    /// Pointer and word-count for the `DT_HASH` (sysv) table, if present.
    /// Used by [`sysv_hash_lookup`] as a fallback when `DT_GNU_HASH` is absent.
    sysv_hash: *const u32,
    sysv_hash_words: usize,
    rels: &'a [Elf64_Rel],
    relas: &'a [Elf64_Rela],
    jmprels: &'a [Elf64_Rela],
    base_address: usize,
}

impl<'a> DynamicInfo<'a> {
    /// Read DT_* entries out of a PT_DYNAMIC array.
    ///
    /// Handles the glibc-vs-musl quirk where glibc stores absolute
    /// addresses in DT entries while musl stores load-relative offsets;
    /// we use the `addr > base ? addr : base + addr` heuristic.
    ///
    /// Supports both `DT_GNU_HASH` and `DT_HASH` (sysv) for determining
    /// the dynsym entry count. Objects with neither hash table fall back
    /// to a symtab/strtab distance heuristic.
    ///
    /// # Safety
    /// - `info` must point to a valid `dl_phdr_info` from `dl_iterate_phdr`.
    /// - The ELF object described by `info` must remain mapped for lifetime `'a`. This is
    ///   guaranteed when called from within a `dl_iterate_phdr` callback (loader lock held) or
    ///   while a `dlopen` handle is live.
    pub unsafe fn from_phdr(info: &'a dl_phdr_info) -> Option<Self> {
        // SAFETY: caller guarantees info is a valid dl_phdr_info for a
        // mapped ELF object. dlpi_phnum is u16 so the conversion is lossless.
        let phdrs = unsafe { slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) };
        let dyn_phdr = phdrs.iter().find(|p| p.p_type == PT_DYNAMIC)?;
        // On 64-bit (this crate's cfg gate), Elf64_Addr (u64) -> usize is lossless.
        let base = u64_to_usize(info.dlpi_addr);
        let dyn_begin = (base + u64_to_usize(dyn_phdr.p_vaddr)) as *const Elf64_Dyn;
        let containing_load_segment_end = |addr: usize| -> Option<usize> {
            phdrs.iter().filter(|p| p.p_type == PT_LOAD).find_map(|p| {
                let start = base.checked_add(u64_to_usize(p.p_vaddr))?;
                let end = start.checked_add(u64_to_usize(p.p_memsz))?;
                (addr >= start && addr < end).then_some(end)
            })
        };
        let correct = |a: u64| -> usize {
            let a = u64_to_usize(a);
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
        let mut sysv_hash: *const u32 = core::ptr::null();
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
                // u64 -> usize: lossless on 64-bit
                DT_STRSZ => strtab_size = u64_to_usize(v),
                DT_SYMTAB => symtab = correct(v) as *const Elf64_Sym,
                DT_GNU_HASH => gnu_hash = correct(v) as *const u32,
                DT_HASH => sysv_hash = correct(v) as *const u32,
                DT_REL => rels = correct(v) as *const Elf64_Rel,
                DT_RELA => relas = correct(v) as *const Elf64_Rela,
                DT_JMPREL => jmprels = correct(v) as *const Elf64_Rela,
                DT_RELSZ => rels_size = u64_to_usize(v),
                DT_RELASZ => relas_size = u64_to_usize(v),
                DT_PLTRELSZ => jmprels_size = u64_to_usize(v),
                // u64 -> i64: reinterpret (tag values fit in i64).
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

        // Need at minimum strtab + symtab to resolve relocation symbol names.
        if strtab.is_null() || symtab.is_null() {
            return None;
        }

        // Compute sysv_hash_words from the containing load segment.
        let sysv_hash_words = if !sysv_hash.is_null() {
            let addr = sysv_hash as usize;
            containing_load_segment_end(addr)
                .and_then(|end| end.checked_sub(addr))
                .map(|bytes| bytes / core::mem::size_of::<u32>())
                .unwrap_or(0)
        } else {
            0
        };

        // Determine sym_count and gnu_hash metadata.
        let (sym_count, gnu_hash_words) = if !gnu_hash.is_null() {
            let gnu_hash_addr = gnu_hash as usize;
            if let Some(end) = containing_load_segment_end(gnu_hash_addr) {
                let bytes = end.saturating_sub(gnu_hash_addr);
                let words = bytes / core::mem::size_of::<u32>();
                if let Some(count) = gnu_hash_symbol_count(gnu_hash, words) {
                    (count, words)
                } else {
                    (sym_count_fallback(symtab, strtab, sysv_hash), words)
                }
            } else {
                (sym_count_fallback(symtab, strtab, sysv_hash), 0)
            }
        } else {
            (sym_count_fallback(symtab, strtab, sysv_hash), 0)
        };

        // SAFETY (applies to all `try_as_slice` calls below):
        //
        // Each pointer was read from the PT_DYNAMIC segment and
        // corrected for the glibc/musl address quirk. The caller
        // guarantees the ELF object remains mapped for lifetime `'a`
        // (safety precondition of from_phdr). The dynamic linker
        // enforces alignment and contiguous mapping at load time.
        // Sizes come from DT_STRSZ, DT_RELSZ, DT_RELASZ, DT_PLTRELSZ,
        // and sym_count (from DT_GNU_HASH or DT_HASH nchain).

        let rels_count = rels_size / core::mem::size_of::<Elf64_Rel>();
        let relas_count = relas_size / core::mem::size_of::<Elf64_Rela>();
        let jmprels_count = jmprels_size / core::mem::size_of::<Elf64_Rela>();

        let strtab_slice = try_as_slice(strtab as *const u8, strtab_size);
        // u32 -> usize: lossless on 64-bit (crate cfg gate).
        let symtab_slice = try_as_slice(symtab, u32_to_usize(sym_count));
        let rels_slice = try_as_slice(rels, rels_count);
        let relas_slice = try_as_slice(relas, relas_count);
        let jmprels_slice = try_as_slice(jmprels, jmprels_count);

        Some(Self {
            strtab: strtab_slice,
            symtab: symtab_slice,
            gnu_hash,
            gnu_hash_words,
            sysv_hash,
            sysv_hash_words,
            rels: rels_slice,
            relas: relas_slice,
            jmprels: jmprels_slice,
            base_address: base,
        })
    }

    /// Look up the symbol entry and its name at index `idx`.
    /// Returns the `Elf64_Sym` and its name from the string table,
    /// or `None` if the index is out of bounds or the name is invalid.
    pub fn sym_entry(&self, idx: u32) -> Option<(&Elf64_Sym, &CStr)> {
        let sym = self.symtab.get(u32_to_usize(idx))?;
        let off = u32_to_usize(sym.st_name);
        let remaining = self.strtab.get(off..)?;
        let nul_pos = remaining.iter().position(|&b| b == 0)?;
        let name = CStr::from_bytes_with_nul(&remaining[..=nul_pos]).ok()?;
        Some((sym, name))
    }

    /// Look up the name of the symbol at index `idx` in the dynamic
    /// string table.
    pub fn sym_name(&self, idx: u32) -> Option<&CStr> {
        self.sym_entry(idx).map(|(_, name)| name)
    }

    /// The base load address of this ELF object.
    pub fn base_address(&self) -> usize {
        self.base_address
    }

    /// REL relocations for this object, or empty if none.
    pub fn rels(&self) -> &[Elf64_Rel] {
        self.rels
    }

    /// RELA relocations for this object, or empty if none.
    pub fn relas(&self) -> &[Elf64_Rela] {
        self.relas
    }

    /// JMPREL (PLT) relocations for this object, or empty if none.
    pub fn jmprels(&self) -> &[Elf64_Rela] {
        self.jmprels
    }

    /// Whether this object has a usable GNU hash table.
    pub fn has_gnu_hash(&self) -> bool {
        !self.gnu_hash.is_null() && self.gnu_hash_words >= 4
    }

    /// Whether this object has a usable sysv (`DT_HASH`) hash table.
    pub fn has_sysv_hash(&self) -> bool {
        !self.sysv_hash.is_null()
    }
}

/// Fallback sym_count determination: try sysv DT_HASH, then
/// symtab/strtab distance heuristic.
///
/// # Safety
/// All non-null pointers must point into the `PT_DYNAMIC` segment of a
/// currently-loaded ELF object (as produced by [`DynamicInfo::from_phdr`]).
unsafe fn sym_count_fallback(
    symtab: *const Elf64_Sym,
    strtab: *const c_char,
    sysv_hash: *const u32,
) -> u32 {
    // DT_HASH (sysv): header is [nbucket, nchain]. nchain == dynsym count.
    if !sysv_hash.is_null() {
        let nchain = *sysv_hash.add(1);
        if nchain > 0 {
            return nchain;
        }
    }

    // Last resort: estimate from the common .dynsym-before-.dynstr layout.
    let symtab_addr = symtab as usize;
    let strtab_addr = strtab as usize;
    if strtab_addr > symtab_addr {
        let bytes = strtab_addr - symtab_addr;
        // usize -> u32: may truncate in theory, but dynsym tables with
        // >4 billion entries don't exist in practice. If it did truncate,
        // sym_name bounds-checks via slice indexing would catch it safely.
        (bytes / core::mem::size_of::<Elf64_Sym>()) as u32
    } else {
        // Can't estimate; allow any index and rely on strtab bounds
        // checking in sym_name to catch bad accesses.
        u32::MAX
    }
}

/// Compute the ELF sysv hash used by `DT_HASH` tables.
/// From <https://refspecs.linuxfoundation.org/elf/elf.pdf>
pub fn sysv_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &c in name {
        // The algorithm is described in C in the spec, and C standard defines arithmetic
        // to be defined modulo `2^N` for N-bits unsigned integers; that is, to be wrapping.
        h = (h << 4).wrapping_add(u32::from(c));
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// Look up a symbol by name in an object's `DT_HASH` (sysv) table.
///
/// ```text
/// [nbucket] [nchain] [bucket[0..nbucket]] [chain[0..nchain]]
/// ```
///
/// Each bucket holds the index of the first symbol in that bucket's
/// chain (or `STN_UNDEF` if empty). Each chain entry at position `i`
/// holds the index of the next symbol after symbol `i` in the same
/// bucket (or `STN_UNDEF` to end the chain). `nchain` equals the
/// total number of dynamic symbols, so chain indices double as
/// symbol indices into `.dynsym`.
///
/// Returns the `Elf64_Sym` entry if found and valid (per [`check_sym`]).
///
/// # Safety
/// `info` must have been produced by [`DynamicInfo::from_phdr`] for a
/// currently-loaded ELF object.
pub unsafe fn sysv_hash_lookup(info: &DynamicInfo, name: &[u8]) -> Option<Elf64_Sym> {
    let hashtab = info.sysv_hash;
    if hashtab.is_null() || info.sysv_hash_words < 2 {
        return None;
    }

    // Read the header: nbucket and nchain.
    let nbucket = *hashtab;
    let nchain = *hashtab.add(1);
    if nbucket == 0 {
        return None;
    }

    // Validate the table fits within the mapped region before computing
    // any pointers into the bucket/chain arrays.
    let buckets_start: usize = 2;
    let chains_start = buckets_start.checked_add(u32_to_usize(nbucket))?;
    let table_end = chains_start.checked_add(u32_to_usize(nchain))?;
    if table_end > info.sysv_hash_words {
        return None;
    }

    let buckets = hashtab.add(buckets_start);
    let chains = hashtab.add(chains_start);

    let h = sysv_hash(name);
    let mut idx = *buckets.add(u32_to_usize(h % nbucket));

    // Follow the chain from the bucket's head symbol, comparing names
    // at each step. The chain terminates at STN_UNDEF (0). We also
    // cap iterations at nchain to guard against malformed cycles.
    let mut steps = 0u32;
    while idx != STN_UNDEF && steps < nchain {
        if (u32_to_usize(idx)) >= u32_to_usize(nchain) {
            break;
        }
        if let Some((sym, sname)) = info.sym_entry(idx) {
            if sname.to_bytes() == name && check_sym(sym) {
                return Some(*sym);
            }
        }
        // SAFETY: idx was bounds-checked above against nchain, and
        // chains points into the validated sysv hash table.
        idx = *chains.add(u32_to_usize(idx));
        steps += 1;
    }
    None
}

/// Compute the GNU symbol hash used by `DT_GNU_HASH` tables.
/// See <https://flapenguin.me/elf-dt-gnu-hash>.
pub fn gnu_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 5381;
    for c in name {
        h = h
            .wrapping_shl(5)
            .wrapping_add(h)
            .wrapping_add(u32::from(*c));
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
    let bloom_size_words = u32_to_usize(bloom_size).checked_mul(2)?;
    let buckets_start = 4usize.checked_add(bloom_size_words)?;
    let chains_start = buckets_start.checked_add(u32_to_usize(nbuckets))?;

    if bloom_size == 0 || buckets_start > hashtab_words || chains_start > hashtab_words {
        return None;
    }
    if nbuckets == 0 {
        return None;
    }

    let buckets = slice::from_raw_parts(hashtab.add(buckets_start), u32_to_usize(nbuckets));
    let mut idx = *buckets.iter().max()?;
    // All buckets empty: hash covers zero defined symbols, but the
    // symtab may still have undefined imports. Signal the caller to
    // use a fallback.
    if idx == STN_UNDEF {
        return None;
    }
    if idx < symbias {
        return None;
    }

    let chain_count = hashtab_words - chains_start;
    loop {
        let chain_idx = u32_to_usize(idx - symbias);
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

    // offset 0: nbuckets     (u32)
    // offset 1: symbias      (u32)  first symbol index covered by the hash
    // offset 2: bloom_size   (u32)  number of u64 bloom filter words
    // offset 3: bloom_shift  (u32)  secondary bloom bit shift
    // offset 4: bloom[bloom_size]   (u64 each, so bloom_size * 2 u32 words)
    //           buckets[nbuckets]   (u32 each)
    //           chains[...]         (u32 each, one per symbol starting at symbias)

    let nbuckets = *hashtab;
    let symbias = *hashtab.add(1);
    let bloom_size = *hashtab.add(2);
    let bloom_shift = *hashtab.add(3);
    let bloom_size_words = u32_to_usize(bloom_size).checked_mul(2)?;
    let buckets_start = 4usize.checked_add(bloom_size_words)?;
    let chains_start = buckets_start.checked_add(u32_to_usize(nbuckets))?;

    if nbuckets == 0
        || bloom_size == 0
        || buckets_start > info.gnu_hash_words
        || chains_start > info.gnu_hash_words
    {
        return None;
    }

    let h = gnu_hash(name);
    let bloom = hashtab.add(4) as *const u64;
    let word = *bloom.add(u32_to_usize((h / 64) & (bloom_size - 1)));
    let bit1 = h & 63;
    let bit2 = (h >> bloom_shift) & 63;
    if ((word >> bit1) & (word >> bit2) & 1) == 0 {
        return None;
    }

    let buckets = hashtab.add(buckets_start);
    let mut symidx = *buckets.add(u32_to_usize(h % nbuckets));
    if symidx == STN_UNDEF {
        return None;
    }
    if symidx < symbias {
        return None;
    }

    let chain_count = info.gnu_hash_words - chains_start;
    loop {
        let chain_idx = u32_to_usize(symidx - symbias);
        if chain_idx >= chain_count {
            return None;
        }
        let chain_h = *hashtab.add(chains_start + chain_idx);
        if ((chain_h ^ h) >> 1) == 0 {
            if let Some((sym, sname)) = info.sym_entry(symidx) {
                if sname.to_bytes() == name && check_sym(sym) {
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

/// Check whether `addr` falls within any of a loaded ELF object's
/// `PT_LOAD` segments. Works regardless of PIE vs non-PIE: on non-PIE
/// executables `dlpi_addr` is 0 but the segments still have the correct
/// absolute virtual addresses once `dlpi_addr` is added.
///
/// # Safety
/// `info` must point to a valid `dl_phdr_info` from `dl_iterate_phdr`.
unsafe fn phdr_contains_addr(info: &dl_phdr_info, addr: usize) -> bool {
    let phdrs = slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
    let base = info.dlpi_addr as usize;
    phdrs.iter().any(|p| {
        if p.p_type != PT_LOAD {
            return false;
        }
        let start = base + p.p_vaddr as usize;
        let end = start + p.p_memsz as usize;
        addr >= start && addr < end
    })
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
        // Treat patching as best-effort and stop iteration on panic.
        result.map(i32::from).unwrap_or(1)
    }

    // SAFETY: `trampoline` has the correct signature for dl_iterate_phdr.
    // `ctx` is live for the duration of the call; the trampoline casts
    // `data` back to `&mut Ctx` and catches panics to prevent unwinding
    // through C frames.
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
        // SAFETY: sysconf(_SC_PAGESIZE) is safe to call; it
        // reads a cached kernel value with no side effects. Returns -1
        // on error; we fall back to 4 KiB in that case.
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
    // Restore every touched page to its original protection. Runs on
    /// scope exit - including panic or early return - so page protections
    /// are never left weakened even if a patching pass bails out midway.
    fn drop(&mut self) {
        for (aligned, orig) in self.touched.drain() {
            // SAFETY: `aligned` was a page-aligned address we successfully
            // mprotect'd earlier; restoring its original protection is safe.
            // Best-effort: nothing sensible to do on failure.
            unsafe { mprotect(aligned as *mut c_void, self.page_size, orig) };
        }
    }
}

/// Extract the symbol index from an ELF64 relocation's `r_info` field.
pub fn elf64_r_sym(info: u64) -> u32 {
    (info >> 32) as u32
}

/// Extract the relocation type from an ELF64 relocation's `r_info` field.
pub fn elf64_r_type(info: u64) -> u32 {
    (info & 0xffff_ffff) as u32
}

/// Return whether the relocation type represents a pointer-width slot
/// that is safe to overwrite with a function pointer.
///
/// Accepted types:
/// - `GLOB_DAT` / `JUMP_SLOT` -- GOT entries filled by the dynamic linker.
/// - `R_X86_64_64` / `R_AARCH64_ABS64` -- absolute pointer-width relocations used for data-section
///   function pointers (`void *(*fn)(size_t) = malloc;`).
///
/// Narrow or PC-relative types (`R_X86_64_PC32`, `R_AARCH64_TLSDESC`, etc.)
/// are excluded since they have different widths and addend semantics
pub fn is_got_pointer_reloc(r_type: u32) -> bool {
    // x86_64
    const R_X86_64_64: u32 = 1;
    const R_X86_64_GLOB_DAT: u32 = 6;
    const R_X86_64_JUMP_SLOT: u32 = 7;
    // aarch64
    const R_AARCH64_ABS64: u32 = 257;
    const R_AARCH64_GLOB_DAT: u32 = 1025;
    const R_AARCH64_JUMP_SLOT: u32 = 1026;

    matches!(
        r_type,
        R_X86_64_64
            | R_X86_64_GLOB_DAT
            | R_X86_64_JUMP_SLOT
            | R_AARCH64_ABS64
            | R_AARCH64_GLOB_DAT
            | R_AARCH64_JUMP_SLOT
    )
}

/// Look up a symbol across loaded objects, returning the first
/// non-zero-sized definition whose address is not `not_this_symbol`.
/// Null-sized symbols are ignored so hooks resolve to callable definitions.
///
/// Uses `gnu_hash_lookup` for objects with `DT_GNU_HASH`, and falls
/// back to `sysv_hash_lookup` for objects that only have `DT_HASH`.
pub fn lookup_symbol(name: &str, not_this_symbol: usize) -> Option<LookupResult> {
    lookup_symbol_impl(name, not_this_symbol, None)
}

/// Like [`lookup_symbol`], but also skips the library whose `PT_LOAD`
/// segments contain `skip_addr`.
///
/// Used by [`hook_symbol_excluding_self`] to ensure `orig_out` resolves
/// to the external definition rather than a same-name export from the
/// hook's own library.
fn lookup_symbol_excluding_addr(
    name: &str,
    not_this_symbol: usize,
    skip_addr: usize,
) -> Option<LookupResult> {
    lookup_symbol_impl(name, not_this_symbol, Some(skip_addr))
}

fn lookup_symbol_impl(
    name: &str,
    not_this_symbol: usize,
    skip_addr: Option<usize>,
) -> Option<LookupResult> {
    let needle = name.as_bytes();
    let mut found: Option<LookupResult> = None;
    // SAFETY: iterate_libraries calls dl_iterate_phdr which guarantees
    // each `info` is a valid dl_phdr_info for a currently-loaded and
    // mapped library. from_phdr's safety precondition (mapped for 'a)
    // is satisfied because the callback runs synchronously under the
    // loader lock.
    iterate_libraries(|info, _is_exe| unsafe {
        let lib_name = if info.dlpi_name.is_null() {
            ""
        } else {
            CStr::from_ptr(info.dlpi_name).to_str().unwrap_or("")
        };
        if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
            return false;
        }
        // Skip the library containing skip_addr (the hook function).
        if let Some(addr) = skip_addr {
            if phdr_contains_addr(info, addr) {
                return false;
            }
        }
        let Some(dyn_info) = DynamicInfo::from_phdr(info) else {
            return false;
        };
        // Try GNU hash, then fall back to sysv hash
        // for objects that only have DT_HASH.
        let sym = if dyn_info.has_gnu_hash() {
            gnu_hash_lookup(&dyn_info, needle)
        } else if dyn_info.has_sysv_hash() {
            sysv_hash_lookup(&dyn_info, needle)
        } else {
            None
        };
        if let Some(sym) = sym {
            if sym.st_size > 0 {
                let addr = u64_to_usize(sym.st_value) + dyn_info.base_address();
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

/// Result of a symbol lookup.
#[derive(Clone, Copy)]
pub struct LookupResult {
    pub address: usize,
}

/// Result of a successful [`hook_symbol`] call.
#[derive(Clone, Copy, Debug)]
pub struct HookResult {
    /// Resolved address of the original symbol. Store this so the hook
    /// function can forward calls to the real implementation.
    pub orig_addr: usize,
    /// Number of GOT entries successfully rewritten.
    pub entries_patched: usize,
    /// Number of GOT entries that matched the symbol but could not be
    /// patched (`mprotect` failed to make the page writable).
    pub entries_failed: usize,
}

/// Error returned by [`hook_symbol`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookError {
    /// `symbol_name` was not valid.
    InvalidSymbolName,
    /// No loaded library exports a definition for this symbol.
    SymbolNotFound,
}

/// Hook a single symbol across all loaded ELF objects by patching their
/// GOT entries, including the library that contains the hook function.
///
/// This is a one-shot API: it patches every library that is loaded at
/// call time. Libraries `dlopen`'d after this call will **not** be
/// patched. Their calls to the hooked symbol will go directly to the
/// original. For hooks that need to cover dynamically loaded libraries,
/// use the lower-level [`DynamicInfo`] / [`iterate_libraries`] /
/// [`PageProtGuard`] primitives to build a registry that re-scans on
/// `dlopen` (see `libdd-profiling-heap-gotter`'s `SymbolOverrides`).
///
/// - `symbol_name`: the symbol to hook (`c"__assert_fail"`)
/// - `hook_fn`: address of the replacement function
///
/// Returns `Ok(HookResult)` if the symbol was resolved, with
/// `orig_addr` set to the original function address and `patched`
/// indicating whether any GOT entries were actually rewritten.
/// Returns `Err(HookError)` if the symbol name is invalid or the
/// symbol could not be found.
///
/// # Safety
///
/// `hook_fn` must point to a function with the same calling convention
/// and signature as the symbol being hooked. The patching is permanent.
pub unsafe fn hook_symbol(symbol_name: &CStr, hook_fn: usize) -> Result<HookResult, HookError> {
    hook_symbol_impl(symbol_name, hook_fn, None)
}

/// Like [`hook_symbol`], but skips the library that contains `hook_fn`.
///
/// Use this when the hook function forwards calls to the original via
/// normal linkage (e.g. `libc::read(fd, buf, len)`) rather than through
/// the stored `orig_addr`. By skipping the hook's own library during
/// patching, its GOT entries remain pointed at the real symbol, so
/// normal calls from within the hook don't recurse.
///
/// This also excludes the hook's library during symbol resolution, so
/// even if it exports the hooked symbol under the same name, `orig_addr`
/// will point to the external definition rather than the hook library's
/// own export.
///
/// # Safety
/// Same as [`hook_symbol`].
pub unsafe fn hook_symbol_excluding_self(
    symbol_name: &CStr,
    hook_fn: usize,
) -> Result<HookResult, HookError> {
    hook_symbol_impl(symbol_name, hook_fn, Some(hook_fn))
}

/// `skip_addr`: if `Some(addr)`, skip the library whose PT_LOAD segments
/// contain `addr`. Used by `hook_symbol_excluding_self` to identify the
/// hook's own library regardless of PIE vs non-PIE (where `dlpi_addr`
/// may be 0 for the main executable).
unsafe fn hook_symbol_impl(
    symbol_name: &CStr,
    hook_fn: usize,
    skip_addr: Option<usize>,
) -> Result<HookResult, HookError> {
    let symbol_name_bytes = symbol_name.to_bytes();
    let name_str = symbol_name
        .to_str()
        .map_err(|_| HookError::InvalidSymbolName)?;

    // Resolve the original symbol, excluding both the hook_fn address
    // and (if excluding self) the entire hook library so we don't
    // accidentally resolve to a different export from the same object.
    let result = if let Some(addr) = skip_addr {
        lookup_symbol_excluding_addr(name_str, hook_fn, addr)
    } else {
        lookup_symbol(name_str, hook_fn)
    }
    .ok_or(HookError::SymbolNotFound)?;

    let mut entries_patched: usize = 0;
    let mut entries_failed: usize = 0;
    let mut guard = PageProtGuard::new();

    let guard_ptr = &mut guard as *mut PageProtGuard;
    let patched_ptr = &mut entries_patched as *mut usize;
    let failed_ptr = &mut entries_failed as *mut usize;

    iterate_libraries(|info, _is_exe| {
        let lib_name = if info.dlpi_name.is_null() {
            ""
        } else {
            // SAFETY: dl_iterate_phdr guarantees dlpi_name is a valid
            // NUL-terminated C string for the callback's duration.
            unsafe { CStr::from_ptr(info.dlpi_name) }
                .to_str()
                .unwrap_or("")
        };
        if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
            return false;
        }

        // Skip the library containing skip_addr (the hook function).
        if let Some(addr) = skip_addr {
            if phdr_contains_addr(info, addr) {
                return false;
            }
        }

        // SAFETY: `info` points to a valid `dl_phdr_info` provided by
        // `dl_iterate_phdr`
        let Some(dyn_info) = (unsafe { DynamicInfo::from_phdr(info) }) else {
            return false;
        };
        // SAFETY: dyn_info was just produced from a currently-loaded
        // library. guard_ptr/patched_ptr/failed_ptr are valid for the
        // duration of iterate_libraries (they point to locals in the
        // enclosing fn).
        unsafe {
            patch_got_entries(
                &dyn_info,
                symbol_name_bytes,
                hook_fn,
                &mut *guard_ptr,
                &mut *patched_ptr,
                &mut *failed_ptr,
            );
        }
        false
    });

    Ok(HookResult {
        orig_addr: result.address,
        entries_patched,
        entries_failed,
    })
}

/// Patch GOT entries in one library for the target symbol.
///
/// Only patches relocations of type `GLOB_DAT`, `JUMP_SLOT`, or
/// pointer-width absolute (`R_X86_64_64` / `R_AARCH64_ABS64`).
/// Narrow or PC-relative relocation types are skipped.
///
/// # Safety
/// `dyn_info` must have been produced by [`DynamicInfo::from_phdr`] for a
/// currently-loaded ELF object. `guard` must belong to the current
/// patching pass.
unsafe fn patch_got_entries(
    dyn_info: &DynamicInfo,
    symbol_name: &[u8],
    hook_fn: usize,
    guard: &mut PageProtGuard,
    patched: &mut usize,
    failed: &mut usize,
) {
    // Both REL and RELA relocations carry r_info (symbol + type) and
    // r_offset (GOT slot address). RELA has an additional r_addend we
    // don't use. This helper processes one relocation by those two fields.
    let mut try_patch = |r_info: u64, r_offset: u64| {
        if !is_got_pointer_reloc(elf64_r_type(r_info)) {
            return;
        }
        let sym_idx = elf64_r_sym(r_info);
        if let Some(cstr) = dyn_info.sym_name(sym_idx) {
            if cstr.to_bytes() == symbol_name {
                let addr = u64_to_usize(r_offset) + dyn_info.base_address();
                if guard.override_entry(addr, hook_fn) {
                    *patched += 1;
                } else {
                    *failed += 1;
                }
            }
        }
    };

    // NOTE: the SysV x86-64 ABI specifies that only RELA entries are
    // used on AMD64 (spec page 64). ARM64 appears similar. REL
    // processing is kept for defensive completeness but may be
    // dead code on both architectures. We should revisit this.
    for reloc in dyn_info.rels() {
        try_patch(reloc.r_info, reloc.r_offset);
    }
    for relocs in [dyn_info.relas(), dyn_info.jmprels()] {
        for reloc in relocs {
            try_patch(reloc.r_info, reloc.r_offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
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
    fn test_gnu_hash_symbol_count_too_small() {
        let data: [u32; 3] = [0; 3];
        assert_eq!(
            unsafe { gnu_hash_symbol_count(data.as_ptr(), data.len()) },
            None
        );
    }

    #[test]
    fn test_gnu_hash_symbol_count_zero_buckets() {
        let data: [u32; 6] = [0, 0, 1, 0, 0, 0];
        assert_eq!(
            unsafe { gnu_hash_symbol_count(data.as_ptr(), data.len()) },
            None,
        );
    }

    #[test]
    fn test_gnu_hash_symbol_count_valid_single_chain() {
        // nbuckets=1, symbias=1, bloom_size=1 (2 u32 words), bloom_shift=0
        // bloom: [0, 0], bucket: [1], chain: [1 (LSB set = end)]
        // sym_count = 1 + 1 = 2
        let data: [u32; 8] = [1, 1, 1, 0, 0, 0, 1, 1];
        assert_eq!(
            unsafe { gnu_hash_symbol_count(data.as_ptr(), data.len()) },
            Some(2),
        );
    }

    #[test]
    fn test_sysv_hash_known_values() {
        // Reference values from the ELF spec and glibc's dl-hash.h.
        // Empty string hashes to 0.
        assert_eq!(sysv_hash(b""), 0);
        // Verify a few known symbol names produce non-zero, distinct hashes.
        let h1 = sysv_hash(b"malloc");
        let h2 = sysv_hash(b"free");
        let h3 = sysv_hash(b"__assert_fail");
        assert!(h1 != 0);
        assert!(h2 != 0);
        assert!(h3 != 0);
        assert!(h1 != h2);
        assert!(h1 != h3);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_iterate_libraries_finds_loaded_objects() {
        let mut count = 0usize;
        iterate_libraries(|_info, _is_exe| {
            count += 1;
            false
        });
        assert!(count > 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_dynamic_info_parses_loaded_library() {
        let mut found = false;
        iterate_libraries(|info, _| {
            if let Some(_dyn_info) = unsafe { DynamicInfo::from_phdr(info) } {
                found = true;
                return true;
            }
            false
        });
        assert!(found, "should parse at least one loaded library");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_all_loaded_libraries_have_valid_sym_count() {
        iterate_libraries(|info, _| {
            // SAFETY: `info` is a valid `dl_phdr_info` from `dl_iterate_phdr`.
            let Some(dyn_info) = (unsafe { DynamicInfo::from_phdr(info) }) else {
                return false;
            };
            assert!(
                !dyn_info.symtab.is_empty(),
                "sym_count should be > 0 (base=0x{:x})",
                dyn_info.base_address
            );
            let name = dyn_info.sym_name(0);
            assert!(
                name.is_some(),
                "sym_name(0) should succeed (base=0x{:x})",
                dyn_info.base_address
            );
            false
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)] // miri doesn't support dl_iterate_phdr
    fn test_can_lookup_known_symbol() {
        let r = lookup_symbol("malloc", 0); // malloc is definitely known
        assert!(r.is_some(), "expected to find malloc in loaded libraries");
        assert!(r.unwrap().address != 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // miri doesn't support dl_iterate_phdr
    fn test_unknown_symbol_lookup_returns_none() {
        let r = lookup_symbol("definitely_not_a_real_libc_symbol_xyzzy", 0);
        assert!(r.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_proc_maps_returns_entries() {
        let maps = read_proc_maps();
        // Every running Linux process has at least a few mappings
        assert!(!maps.is_empty(), "read_proc_maps should return entries");

        for entry in &maps {
            // Every mapping has a non-zero size.
            assert!(
                entry.end > entry.start,
                "mapping {:#x}-{:#x} has zero or negative size",
                entry.start,
                entry.end,
            );
            // Prot flags should only contain the bits we parse.
            let valid_bits = PROT_READ | PROT_WRITE | PROT_EXEC;
            assert!(
                entry.prot & !valid_bits == 0,
                "unexpected prot bits {:#x} in mapping {:#x}-{:#x}",
                entry.prot,
                entry.start,
                entry.end,
            );
        }

        // At least one mapping should be readable (the executable itself).
        assert!(
            maps.iter().any(|e| e.prot & PROT_READ != 0),
            "expected at least one readable mapping"
        );
    }

    /// Compile a tiny shared library with `--hash-style=sysv` (no
    /// DT_GNU_HASH), dlopen it, and verify that `DynamicInfo::from_phdr`
    /// parses it with a valid sym_count calculated from DT_HASH nchain.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_sysv_hash_library_parsed_correctly() {
        use std::io::Write;
        use std::process::Command;

        let dir = TempDir::new().expect("create temp dir");
        let c_path = dir.path().join("sysv_test.c");
        let so_path = dir.path().join("libsysv_test.so");

        {
            let mut f = std::fs::File::create(&c_path).expect("create .c");
            f.write_all(b"int sysv_test_symbol(void) { return 42; }\n")
                .expect("write .c");
        }

        // Compile with --hash-style=sysv so the .so has DT_HASH but no
        // DT_GNU_HASH.
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-Wl,--hash-style=sysv", "-o"])
            .arg(&so_path)
            .arg(&c_path)
            .status();

        let status = status.expect("cc should be available");
        assert!(
            status.success(),
            "cc --hash-style=sysv compilation failed: {status}"
        );

        // dlopen the library.
        let so_cstr =
            std::ffi::CString::new(so_path.to_str().expect("path is utf8")).expect("CString");
        let handle = unsafe { libc::dlopen(so_cstr.as_ptr(), libc::RTLD_NOW) };
        assert!(!handle.is_null(), "dlopen failed: {:?}", unsafe {
            CStr::from_ptr(libc::dlerror())
        },);

        // Walk loaded libraries and find our .so.
        let mut found = false;
        iterate_libraries(|info, _| {
            let lib_name = if info.dlpi_name.is_null() {
                return false;
            } else {
                unsafe { CStr::from_ptr(info.dlpi_name) }
                    .to_str()
                    .unwrap_or("")
            };
            if !lib_name.contains("libsysv_test") {
                return false;
            }

            let dyn_info = unsafe { DynamicInfo::from_phdr(info) };
            assert!(
                dyn_info.is_some(),
                "from_phdr should succeed for sysv-hash library at {lib_name}"
            );
            let dyn_info = dyn_info.unwrap();
            assert!(
                !dyn_info.symtab.is_empty(),
                "sym_count should be > 0 for sysv-hash library"
            );

            // Verify we can look up our exported symbol by walking
            // relocations aren't needed
            let mut found_sym = false;
            for idx in 0..dyn_info.symtab.len() as u32 {
                if let Some(name) = dyn_info.sym_name(idx) {
                    if name.to_bytes() == b"sysv_test_symbol" {
                        found_sym = true;
                        break;
                    }
                }
            }
            assert!(
                found_sym,
                "should find sysv_test_symbol in dynsym (sym_count={})",
                dyn_info.symtab.len()
            );

            found = true;
            true
        });

        assert!(
            found,
            "should have found libsysv_test.so in loaded libraries"
        );

        // Verify that lookup_symbol (which uses sysv_hash_lookup for
        // DT_HASH-only objects) can resolve the exported symbol.
        let result = lookup_symbol("sysv_test_symbol", 0);
        assert!(
            result.is_some(),
            "lookup_symbol should find sysv_test_symbol via sysv hash lookup"
        );
        assert!(result.unwrap().address != 0);

        unsafe { libc::dlclose(handle) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_got_pointer_reloc_accepts_pointer_width_types() {
        // x86_64
        assert!(is_got_pointer_reloc(1)); // R_X86_64_64
        assert!(is_got_pointer_reloc(6)); // R_X86_64_GLOB_DAT
        assert!(is_got_pointer_reloc(7)); // R_X86_64_JUMP_SLOT
                                          // aarch64
        assert!(is_got_pointer_reloc(257)); // R_AARCH64_ABS64
        assert!(is_got_pointer_reloc(1025)); // R_AARCH64_GLOB_DAT
        assert!(is_got_pointer_reloc(1026)); // R_AARCH64_JUMP_SLOT
    }

    #[test]
    fn test_is_got_pointer_reloc_rejects_non_pointer_types() {
        assert!(!is_got_pointer_reloc(0)); // R_*_NONE
        assert!(!is_got_pointer_reloc(2)); // R_X86_64_PC32
        assert!(!is_got_pointer_reloc(10)); // R_X86_64_32
        assert!(!is_got_pointer_reloc(11)); // R_X86_64_32S
        assert!(!is_got_pointer_reloc(258)); // R_AARCH64_ABS32
        assert!(!is_got_pointer_reloc(1029)); // R_AARCH64_TLSDESC
        assert!(!is_got_pointer_reloc(u32::MAX));
    }

    /// Verify that `hook_symbol_excluding_self` skips the library
    /// containing the hook function. Uses `phdr_contains_addr` to identify
    /// the hook's own library. Due to rust's testing infrastructure, this test only
    /// covers PIE executables.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_hook_symbol_excluding_self_skips_own_library() {
        // Use a dummy hook function defined in this test binary.
        unsafe extern "C" fn dummy_hook() {}
        let hook_addr = dummy_hook as *const () as usize;

        // Count how many libraries would be visited with and without
        // the self-skip.
        let mut total_libs = 0usize;
        let mut libs_excluding_self = 0usize;

        iterate_libraries(|info, _| {
            let lib_name = if info.dlpi_name.is_null() {
                ""
            } else {
                unsafe { CStr::from_ptr(info.dlpi_name) }
                    .to_str()
                    .unwrap_or("")
            };
            if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
                return false;
            }
            if unsafe { DynamicInfo::from_phdr(info) }.is_none() {
                return false;
            }
            total_libs += 1;
            if !unsafe { phdr_contains_addr(info, hook_addr) } {
                libs_excluding_self += 1;
            }
            false
        });

        assert!(total_libs > 0, "should find at least one library");
        assert!(
            libs_excluding_self < total_libs,
            "excluding self should skip at least one library \
             (total={total_libs}, excluding_self={libs_excluding_self})"
        );
    }

    /// Sanity check against real loaded libraries: the filter should
    /// accept some relocations (GOT entries exist) and reject some
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_is_got_pointer_reloc_filters_real_relocations() {
        let mut accepted = 0usize;
        let mut rejected = 0usize;

        iterate_libraries(|info, _| {
            // SAFETY: `info` is a valid `dl_phdr_info` from `dl_iterate_phdr`.
            let Some(dyn_info) = (unsafe { DynamicInfo::from_phdr(info) }) else {
                return false;
            };
            for relocs in [dyn_info.relas(), dyn_info.jmprels()] {
                for reloc in relocs {
                    if is_got_pointer_reloc(elf64_r_type(reloc.r_info)) {
                        accepted += 1;
                    } else {
                        rejected += 1;
                    }
                }
            }
            false
        });

        assert!(
            accepted > 0,
            "expected at least one GOT relocation across loaded libraries"
        );
        assert!(
            rejected > 0,
            "expected at least one non-GOT relocation to be filtered out"
        );
    }
}
