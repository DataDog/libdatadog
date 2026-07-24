// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! single symbol GOT hooker for 64-bit Linux.
//!
//! Walks every loaded ELF object using `dl_iterate_phdr`, parses its
//! `PT_DYNAMIC` for the symbol/string tables and the relocation arrays,
//! and rewrites GOT entries whose symbol name matches the target.
//!
//! This is a simplified version of the algorithm used by
//! `libdd-profiling-heap-gotter/src/elf.rs`. It only patches a single
//! symbol per call.

#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use core::ffi::{c_char, c_int, c_void, CStr};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use libc::{
    dl_iterate_phdr, dl_phdr_info, mprotect, sysconf, Elf64_Rel, Elf64_Rela, Elf64_Sym, PROT_EXEC,
    PROT_READ, PROT_WRITE, PT_DYNAMIC, PT_LOAD, _SC_PAGESIZE,
};

#[allow(non_camel_case_types)]
#[repr(C)]
struct Elf64_Dyn {
    d_tag: i64,
    d_un: u64,
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

/// Parsed PT_DYNAMIC segment
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
    base_address: usize,
}

impl DynamicInfo {
    /// Parse PT_DYNAMIC entries from a loaded ELF object.
    ///
    /// Handles the glibc-vs-musl quirk: glibc stores absolute addresses in
    /// DT entries, musl stores load-relative offsets. The `correct` helper
    /// uses a heuristic (`addr > base ? addr : base + addr`) to normalise.
    ///
    /// # Safety
    /// `info` must point to a valid `dl_phdr_info` from `dl_iterate_phdr`.
    unsafe fn from_phdr(info: &dl_phdr_info) -> Option<Self> {
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
            let symtab_addr = symtab as usize;
            let strtab_addr = strtab as usize;
            if strtab_addr > symtab_addr {
                let bytes = strtab_addr - symtab_addr;
                (bytes / core::mem::size_of::<Elf64_Sym>()) as u32
            } else {
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
            base_address: base,
        })
    }

    /// Get the name of the symbol at index `idx` from the dynamic string table.
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

/// Compute the total symbol count from a `.gnu.hash` table.
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
        return None;
    }

    let buckets = core::slice::from_raw_parts(hashtab.add(buckets_start), nbuckets as usize);
    let mut idx = *buckets.iter().max()?;
    if idx == 0 {
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

fn iterate_libraries(mut callback: impl FnMut(&dl_phdr_info) -> bool) {
    struct Ctx<'a> {
        callback: &'a mut dyn FnMut(&dl_phdr_info) -> bool,
    }
    let mut ctx = Ctx {
        callback: &mut callback,
    };

    unsafe extern "C" fn trampoline(
        info: *mut dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        let result = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
            let ctx = &mut *(data as *mut Ctx);
            (ctx.callback)(&*info)
        }));
        result.map(i32::from).unwrap_or(1)
    }

    unsafe {
        dl_iterate_phdr(Some(trampoline), &mut ctx as *mut _ as *mut c_void);
    }
}

#[derive(Clone, Copy)]
struct MapEntry {
    start: usize,
    end: usize,
    prot: i32,
}

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

/// Restores each touched page to its original protection on drop.
struct PageProtGuard {
    page_size: usize,
    maps: Vec<MapEntry>,
    touched: HashMap<usize, i32>,
}

impl PageProtGuard {
    fn new() -> Self {
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

    unsafe fn override_entry(&mut self, addr: usize, new_value: usize) -> bool {
        let aligned = addr & !(self.page_size - 1);
        if !self.touched.contains_key(&aligned) {
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

impl Drop for PageProtGuard {
    fn drop(&mut self) {
        for (aligned, orig) in self.touched.drain() {
            unsafe { mprotect(aligned as *mut c_void, self.page_size, orig) };
        }
    }
}

fn elf64_r_sym(info: u64) -> u32 {
    (info >> 32) as u32
}

/// Hook a single symbol across all loaded ELF objects by patching their
/// GOT entries.
///
/// # Safety
/// `hook_fn` must point to a function with the same calling convention and
/// signature as the symbol being hooked. The patching is permanent.
pub(crate) unsafe fn hook_symbol(
    symbol_name_c: &CStr,
    symbol_name_bytes: &[u8],
    hook_fn: usize,
    orig_out: &mut usize,
) -> bool {
    // Resolve the real symbol address with dlsym(RTLD_DEFAULT).
    let real = libc::dlsym(libc::RTLD_DEFAULT, symbol_name_c.as_ptr());
    if real.is_null() {
        return false;
    }
    let real_addr = real as usize;
    if real_addr == hook_fn {
        // Our hook is already the default resolution so nothing to patch.
        return false;
    }
    *orig_out = real_addr;

    // Walk all loaded libraries and patch GOT entries for this symbol.
    let mut patched_any = false;
    let mut guard = PageProtGuard::new();

    // We need raw pointers because the closure passed to iterate_libraries
    // borrows these mutably, and iterate_libraries takes a FnMut.
    let guard_ptr = &mut guard as *mut PageProtGuard;
    let patched_ptr = &mut patched_any as *mut bool;

    iterate_libraries(|info| {
        let lib_name = if info.dlpi_name.is_null() {
            ""
        } else {
            // SAFETY: dl_iterate_phdr guarantees dlpi_name is a valid C string.
            unsafe { CStr::from_ptr(info.dlpi_name) }
                .to_str()
                .unwrap_or("")
        };
        if lib_name.contains("linux-vdso") || lib_name.contains("/ld-linux") {
            return false;
        }
        let Some(dyn_info) = (unsafe { DynamicInfo::from_phdr(info) }) else {
            return false;
        };
        // SAFETY: dyn_info was just parsed from a currently-loaded library.
        // guard_ptr and patched_ptr are valid for the duration of
        // iterate_libraries.
        unsafe {
            patch_got_entries(
                &dyn_info,
                symbol_name_bytes,
                hook_fn,
                &mut *guard_ptr,
                &mut *patched_ptr,
            );
        }
        false // continue
    });

    // guard drops here
    patched_any
}

/// Patch GOT entries in one library for the target symbol.
unsafe fn patch_got_entries(
    dyn_info: &DynamicInfo,
    symbol_name: &[u8],
    hook_fn: usize,
    guard: &mut PageProtGuard,
    patched: &mut bool,
) {
    // Process REL relocations
    if !dyn_info.rels.is_null() {
        let relocs = core::slice::from_raw_parts(dyn_info.rels, dyn_info.rels_count);
        for reloc in relocs {
            let sym_idx = elf64_r_sym(reloc.r_info);
            if let Some(cstr) = dyn_info.sym_name(sym_idx) {
                if cstr.to_bytes() == symbol_name {
                    let addr = reloc.r_offset as usize + dyn_info.base_address;
                    if guard.override_entry(addr, hook_fn) {
                        *patched = true;
                    }
                }
            }
        }
    }

    // Process RELA and JMPREL relocations
    for (ptr, count) in [
        (dyn_info.relas, dyn_info.relas_count),
        (dyn_info.jmprels, dyn_info.jmprels_count),
    ] {
        if ptr.is_null() {
            continue;
        }
        let relocs = core::slice::from_raw_parts(ptr, count);
        for reloc in relocs {
            let sym_idx = elf64_r_sym(reloc.r_info);
            if let Some(cstr) = dyn_info.sym_name(sym_idx) {
                if cstr.to_bytes() == symbol_name {
                    let addr = reloc.r_offset as usize + dyn_info.base_address;
                    if guard.override_entry(addr, hook_fn) {
                        *patched = true;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)] // miri doesn't support dl_iterate_phdr
    fn test_iterate_libraries_finds_loaded_objects() {
        let mut count = 0usize;
        iterate_libraries(|_info| {
            count += 1;
            false
        });
        assert!(
            count > 0,
            "dl_iterate_phdr should find at least one loaded object"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // miri doesn't support dl_iterate_phdr
    fn test_dynamic_info_parses_loaded_library() {
        let mut found = false;
        iterate_libraries(|info| {
            if let Some(_dyn_info) = unsafe { DynamicInfo::from_phdr(info) } {
                found = true;
                return true;
            }
            false
        });
        assert!(found, "should parse at least one loaded library");
    }
}
