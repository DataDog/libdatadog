// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read an OTel `ThreadContextRecord` from a remote process via `/proc/<pid>/mem`.

use alloc::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::crash_info::{OtelAttribute, OtelThreadContext};

/// Size of the fixed record header (trace_id + span_id + valid + trace_flags + attrs_data_size).
pub(crate) const RECORD_HEADER_SIZE: usize = 28;

/// Maximum `attrs_data` the spec allows (keeps the total record at or under 640 bytes).
const MAX_ATTRS_DATA: usize = 612;

#[derive(Debug)]
pub(crate) enum OtelTlsError {
    /// I/O error reading `/proc/<pid>/mem`.
    Io(std::io::Error),
    /// The DTV pointer read from the thread's TCB was null.
    DtvNull,
    /// The DTV entry for the TLS module is unallocated (the thread has never accessed it).
    TlsNotAllocated,
}

impl fmt::Display for OtelTlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::DtvNull => write!(f, "DTV pointer is null"),
            Self::TlsNotAllocated => write!(f, "TLS block not allocated for this thread"),
        }
    }
}

impl From<std::io::Error> for OtelTlsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reader for a remote process's memory via `/proc/<pid>/mem`.
///
/// Opens the file once; all subsequent reads reuse the same file descriptor.
pub(crate) struct ProcMemReader {
    file: File,
}

impl ProcMemReader {
    pub fn open(pid: u32) -> Result<Self, OtelTlsError> {
        Ok(Self {
            file: File::open(format!("/proc/{pid}/mem"))?,
        })
    }

    /// Read exactly `buf.len()` bytes at `addr`. Errors on short reads.
    fn read_exact(&mut self, addr: u64, buf: &mut [u8]) -> Result<(), OtelTlsError> {
        self.file.seek(SeekFrom::Start(addr))?;
        self.file.read_exact(buf)?;
        Ok(())
    }

    /// Best-effort read: returns the number of bytes actually read.
    /// `/proc/<pid>/mem` produces short reads at unmapped page boundaries;
    /// this returns whatever data is available rather than erroring.
    #[allow(clippy::std_instead_of_core)] // core::io::ErrorKind is unstable
    fn read_up_to(&mut self, addr: u64, buf: &mut [u8]) -> Result<usize, OtelTlsError> {
        self.file.seek(SeekFrom::Start(addr))?;
        // A single read(2) on /proc/<pid>/mem returns all readable bytes up to
        // the next unmapped page boundary. Retry only on EINTR.
        loop {
            match self.file.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Read a native-endian `u64` at `addr`.
    fn read_u64(&mut self, addr: u64) -> Result<u64, OtelTlsError> {
        let mut buf = [0u8; 8];
        self.read_exact(addr, &mut buf)?;
        Ok(u64::from_ne_bytes(buf))
    }

    /// Read the 8-byte TLS slot value (a pointer to the record).
    /// Returns `Ok(0)` if the slot is null (no context attached on that thread).
    pub fn read_tls_slot(&mut self, slot_addr: u64) -> Result<u64, OtelTlsError> {
        self.read_u64(slot_addr)
    }

    /// Resolve a TLS variable's address for a remote thread by walking its DTV.
    ///
    /// Works for both static TLS (initial-exec / local-exec) and dynamic TLS
    /// (global-dynamic / TLSDESC backed by `__tls_get_addr`).
    ///
    /// `dtv_offset` is the signed byte offset from TP to the DTV pointer
    /// (see [`validated_dtv_offset`]).
    ///
    /// # DTV entry layout
    ///
    /// - **glibc**: 16-byte `dtv_t` entries. `dtv[modid].pointer.val` = TLS block base.
    /// - **musl**: 8-byte `uintptr_t` entries. `dtv[modid]` = TLS block base.
    pub fn resolve_tls_addr(
        &mut self,
        tp: u64,
        dtv_offset: isize,
        modid: usize,
        offset: usize,
    ) -> Result<u64, OtelTlsError> {
        let Some(dtv_addr) = tp.checked_add_signed(dtv_offset as i64) else {
            return Err(OtelTlsError::DtvNull);
        };
        let dtv_ptr = self.read_u64(dtv_addr)?;
        if dtv_ptr == 0 {
            return Err(OtelTlsError::DtvNull);
        }

        #[cfg(target_env = "gnu")]
        const DTV_ENTRY_SIZE: u64 = 16;
        #[cfg(target_env = "musl")]
        const DTV_ENTRY_SIZE: u64 = 8;

        let tls_block_base = self.read_u64(dtv_ptr + (modid as u64) * DTV_ENTRY_SIZE)?;

        if tls_block_base == u64::MAX || tls_block_base == 0 {
            return Err(OtelTlsError::TlsNotAllocated);
        }

        Ok(tls_block_base + offset as u64)
    }

    /// Two-phase read of an OTel thread-context record.
    ///
    /// 1. Read the 28-byte header (exact) to get `valid` and `attrs_data_size`.
    /// 2. Read up to `attrs_data_size` attribute bytes (best-effort / short-read tolerant).
    ///
    /// `buf` must be at least [`RECORD_HEADER_SIZE`] bytes. Only the bytes actually
    /// read are touched; the rest is left unchanged.
    ///
    /// Returns `Ok(Some(n))` with the number of valid bytes, or `Ok(None)` if
    /// the record is not valid (e.g. mid-update).
    pub fn read_record(
        &mut self,
        record_addr: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, OtelTlsError> {
        debug_assert!(buf.len() >= RECORD_HEADER_SIZE);

        self.read_exact(record_addr, &mut buf[..RECORD_HEADER_SIZE])?;

        if buf[24] != 1 {
            return Ok(None);
        }

        let attrs_data_size = u16::from_ne_bytes([buf[26], buf[27]]) as usize;
        let attrs_to_read = attrs_data_size
            .min(MAX_ATTRS_DATA)
            .min(buf.len() - RECORD_HEADER_SIZE);

        let attrs_read = if attrs_to_read > 0 {
            self.read_up_to(
                record_addr + RECORD_HEADER_SIZE as u64,
                &mut buf[RECORD_HEADER_SIZE..RECORD_HEADER_SIZE + attrs_to_read],
            )?
        } else {
            0
        };

        Ok(Some(RECORD_HEADER_SIZE + attrs_read))
    }
}

/// TLS module metadata discovered via `dl_iterate_phdr`.
pub(crate) struct TlsModuleInfo {
    /// TLS module ID (index into the DTV).
    pub modid: usize,
    /// Byte offset of the `otel_thread_ctx_v1` slot within the module's TLS block.
    pub offset: usize,
    /// The calling thread's TLS block base for this module (`dlpi_tls_data`).
    /// Equal to `dtv[modid]` for the calling thread.
    ///
    /// `None` when `dlpi_tls_data` is NULL — this happens when the calling thread has never
    /// accessed TLS from a `dlopen`'d library. The DTV walk for *remote* threads can still
    /// succeed via `modid` + `offset`; only DTV offset validation (which compares against the
    /// calling thread's block) is unavailable.
    pub tls_block_base: Option<usize>,
}

// ELF constants for parsing the dynamic symbol table. These are ABI-stable values from the
// ELF specification.
const DT_NULL: u64 = 0;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_GNU_HASH: u64 = 0x6fff_fef5;
const STT_TLS: u8 = 6;
const SHN_UNDEF: u16 = 0;

/// 64-bit ELF dynamic section entry.
#[repr(C)]
struct Elf64Dyn {
    d_tag: i64,
    d_val: u64,
}

/// 64-bit ELF symbol table entry.
#[repr(C)]
struct Elf64Sym {
    st_name: u32,
    st_info: u8,
    _st_other: u8,
    st_shndx: u16,
    st_value: u64,
    _st_size: u64,
}

/// Whether `DT_SYMTAB`, `DT_STRTAB`, and hash-table `d_val` fields are object-relative
/// virtual addresses (musl) or already-relocated pointers (glibc).
///
/// glibc's loader relocates these entries in-place during `_dl_relocate_object`, so they are
/// absolute virtual addresses by the time `dl_iterate_phdr` runs. musl stores the raw `p_vaddr`
/// values and adds the load address at lookup time. We therefore need to add `dlpi_addr` on musl
/// to get valid pointers.
#[cfg(target_env = "musl")]
const fn resolve_dyn_ptr(d_val: u64, load_addr: u64) -> usize {
    (load_addr + d_val) as usize
}

#[cfg(not(target_env = "musl"))]
const fn resolve_dyn_ptr(d_val: u64, _load_addr: u64) -> usize {
    d_val as usize
}

const SYMBOL_NAME: &[u8] = b"otel_thread_ctx_v1";

/// Search the dynamic symbol table of a loaded ELF object for `otel_thread_ctx_v1`.
///
/// Returns the symbol's `st_value` (TLS offset within the module's TLS block) if found,
/// or `None` if the symbol is not present or is not a TLS symbol.
///
/// # Safety
///
/// `base_addr` must be the load address of a currently-loaded ELF object whose program
/// headers are valid and readable. Called from within a `dl_iterate_phdr` callback where the
/// loader lock is held.
unsafe fn find_tls_symbol_offset(phdrs: &[libc::Elf64_Phdr], base_addr: u64) -> Option<usize> {
    // 1. Find PT_DYNAMIC.
    let dyn_phdr = phdrs.iter().find(|p| p.p_type == libc::PT_DYNAMIC)?;
    let dyn_addr = base_addr + dyn_phdr.p_vaddr;

    // 2. Walk dynamic entries to find DT_SYMTAB, DT_STRTAB, and a hash table.
    let mut symtab: *const Elf64Sym = core::ptr::null();
    let mut strtab: *const u8 = core::ptr::null();
    let mut nsyms: usize = 0;
    let mut gnu_hash_ptr: *const u32 = core::ptr::null();

    let mut dyn_ptr = dyn_addr as *const Elf64Dyn;
    loop {
        // SAFETY: the dynamic section is a DT_NULL-terminated array of Elf64_Dyn entries
        // within the mapped pages of the loaded object. We start at the address derived from
        // PT_DYNAMIC and advance one entry at a time, stopping at DT_NULL. The loader lock
        // held by `dl_iterate_phdr` prevents concurrent unloading.
        let entry = unsafe { &*dyn_ptr };
        if entry.d_tag == DT_NULL as i64 {
            break;
        }
        match entry.d_tag as u64 {
            DT_SYMTAB => symtab = resolve_dyn_ptr(entry.d_val, base_addr) as *const Elf64Sym,
            DT_STRTAB => strtab = resolve_dyn_ptr(entry.d_val, base_addr) as *const u8,
            DT_HASH => {
                // ELF hash table layout: [ nbucket: u32, nchain: u32, ... ]
                // `nchain` equals the number of symbols in `.dynsym`.
                let hash_ptr = resolve_dyn_ptr(entry.d_val, base_addr) as *const u32;
                // SAFETY: the ELF hash table is within the mapped pages of the loaded
                // object and always starts with two u32 fields (nbucket and nchain).
                nsyms = unsafe { *hash_ptr.add(1) } as usize;
            }
            DT_GNU_HASH => {
                gnu_hash_ptr = resolve_dyn_ptr(entry.d_val, base_addr) as *const u32;
            }
            _ => {}
        }
        // SAFETY: advancing within the dynamic section; the loop terminates at DT_NULL.
        dyn_ptr = unsafe { dyn_ptr.add(1) };
    }

    if symtab.is_null() || strtab.is_null() {
        return None;
    }

    // If DT_HASH was not present, derive the symbol count from DT_GNU_HASH.
    //
    // GNU hash layout (all fields native-endian u32 unless noted):
    //   [ nbuckets, symoffset, bloom_size, bloom_shift,
    //     bloom[bloom_size] (u64 entries),
    //     buckets[nbuckets],
    //     chains[...] ]
    //
    // The highest populated symbol index is `max(buckets[i])`. From there, walk the
    // chain array until bit 0 is set (end-of-chain marker). `nsyms = last_index + 1`.
    if nsyms == 0 && !gnu_hash_ptr.is_null() {
        // SAFETY: DT_GNU_HASH points to a valid GNU hash table header (4 × u32) within
        // the mapped pages of the loaded object.
        let nbuckets = unsafe { *gnu_hash_ptr } as usize;
        let symoffset = unsafe { *gnu_hash_ptr.add(1) } as usize;
        let bloom_size = unsafe { *gnu_hash_ptr.add(2) } as usize;

        // SAFETY: the bloom filter is `bloom_size` u64 entries (= `bloom_size * 2` u32
        // slots) starting at header offset 4. The buckets array follows immediately.
        let buckets_ptr = unsafe { gnu_hash_ptr.add(4 + bloom_size * 2) };
        // SAFETY: the chains array follows the buckets array.
        let chains_ptr = unsafe { buckets_ptr.add(nbuckets) };

        // Find the maximum bucket value (highest starting symbol index).
        let mut max_sym = 0usize;
        for i in 0..nbuckets {
            // SAFETY: `i < nbuckets`, so `buckets_ptr.add(i)` is within the buckets array.
            let bucket = unsafe { *buckets_ptr.add(i) } as usize;
            if bucket > max_sym {
                max_sym = bucket;
            }
        }

        // Walk the chain from max_sym to find the last symbol.
        if max_sym >= symoffset {
            let mut idx = max_sym;
            loop {
                // SAFETY: `idx - symoffset` is within the chains array; the chain is
                // terminated by a value with bit 0 set, so this loop is bounded.
                let chain_val = unsafe { *chains_ptr.add(idx - symoffset) };
                if chain_val & 1 != 0 {
                    break; // End-of-chain marker.
                }
                idx += 1;
            }
            nsyms = idx + 1;
        }
    }

    if nsyms == 0 {
        return None;
    }

    // 3. Linear scan of the symbol table.
    for i in 0..nsyms {
        // SAFETY: `i < nsyms` and `.dynsym` has at least `nsyms` entries (derived from
        // DT_HASH's nchain or the DT_GNU_HASH chain walk above).
        let sym = unsafe { &*symtab.add(i) };
        if sym.st_info & 0xf != STT_TLS {
            continue;
        }
        // Skip undefined (imported) TLS symbols — they have st_value == 0 and would
        // incorrectly match against the importing module's TLS block rather than the
        // defining library's.
        if sym.st_shndx == SHN_UNDEF {
            continue;
        }
        // SAFETY: `st_name` is an index into the `.dynstr` string table, which is within
        // the mapped pages of the loaded object. The string is NUL-terminated per ELF spec.
        let name_ptr = unsafe { strtab.add(sym.st_name as usize) };
        // Compare the symbol name byte-by-byte up to and including the NUL terminator.
        // SAFETY: we read at most `SYMBOL_NAME.len() + 1` bytes from the NUL-terminated
        // string starting at `name_ptr`, which is within the `.dynstr` section.
        let matches = SYMBOL_NAME
            .iter()
            .enumerate()
            .all(|(j, &b)| unsafe { *name_ptr.add(j) == b })
            && unsafe { *name_ptr.add(SYMBOL_NAME.len()) == 0 };
        if matches {
            return Some(sym.st_value as usize);
        }
    }

    None
}

/// Discover TLS module metadata for `otel_thread_ctx_v1` via `dl_iterate_phdr`.
///
/// Scans the dynamic symbol table of each loaded ELF object for a `STT_TLS` symbol named
/// `otel_thread_ctx_v1`. This avoids TLSDESC relocations (which can overflow in large ThinLTO
/// binaries) and works in both statically and dynamically linked builds.
///
/// Returns `None` if the symbol is not found or `dlpi_tls_data` is NULL
pub(crate) fn tls_module_info() -> Option<TlsModuleInfo> {
    struct State {
        result: Option<TlsModuleInfo>,
    }

    unsafe extern "C" fn callback(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut core::ffi::c_void,
    ) -> core::ffi::c_int {
        // SAFETY: `data` points to the `State` local in the enclosing function, which is
        // live for the duration of the `dl_iterate_phdr` call.
        let state = unsafe { &mut *(data as *mut State) };
        // SAFETY: `info` is a valid `dl_phdr_info` provided by `dl_iterate_phdr`.
        let info = unsafe { &*info };

        // SAFETY: `dl_iterate_phdr` holds the loader lock, so the program headers are valid.
        let phdrs =
            unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };

        // SAFETY: `info.dlpi_addr` is the load address of a currently-loaded ELF object.
        if let Some(offset) = unsafe { find_tls_symbol_offset(phdrs, info.dlpi_addr) } {
            // `dlpi_tls_data` may be NULL if the calling thread has never accessed TLS from
            // this module (e.g. a dlopen'd library used only by worker threads). We still
            // record modid + offset so the DTV walk can resolve remote threads; only the
            // DTV offset validation (which needs the calling thread's block base) is skipped.
            let tls_block_base = if info.dlpi_tls_data.is_null() {
                None
            } else {
                Some(info.dlpi_tls_data as usize)
            };
            state.result = Some(TlsModuleInfo {
                modid: info.dlpi_tls_modid,
                offset,
                tls_block_base,
            });
            return 1; // stop iteration
        }

        0 // continue
    }

    let mut state = State { result: None };

    // SAFETY: `callback` has the correct signature for `dl_iterate_phdr`. `state` is live
    // for the duration of the call and passed as an opaque pointer.
    unsafe {
        libc::dl_iterate_phdr(
            Some(callback),
            &mut state as *mut State as *mut core::ffi::c_void,
        );
    }

    state.result
}

/// Return the signed byte offset from the thread pointer (TP) to the DTV pointer for the
/// current (architecture, libc) combination.
///
/// Referred to the offsets in GDB's `get_tls_dtv_addr` implementation
///
/// GDB references:
/// - x86_64: <https://sourceware.org/git/?p=binutils-gdb.git;a=blob;f=gdb/amd64-linux-tdep.c>
///   (`amd64_linux_get_tls_dtv_addr`)
/// - aarch64: <https://sourceware.org/git/?p=binutils-gdb.git;a=blob;f=gdb/aarch64-linux-tdep.c>
///   (`aarch64_linux_get_tls_dtv_addr`)
///
/// - **x86_64 glibc**: `+8` — second field of `tcbhead_t` (`{void *tcb; dtv_t *dtv; …}`), glibc
///   `sysdeps/x86_64/nptl/tls.h`
/// - **aarch64 glibc**: `+0` — first field of `tcbhead_t` (`{dtv_t *dtv; …}`), glibc
///   `sysdeps/aarch64/nptl/tls.h`
/// - **x86_64 musl**: `+8` — second field of `struct __pthread` (`{self, *dtv, …}`), musl
///   `src/internal/pthread_impl.h` with `!TLS_ABOVE_TP`
/// - **aarch64 musl**: `-8` — TP points just past the end of `struct __pthread` (`TP_ADJ` adds
///   `sizeof(struct __pthread)`), and `*dtv` is the last field, musl `src/internal/pthread_impl.h`
///   with `TLS_ABOVE_TP`
///
/// Returns `None` for unknown combinations (the caller should fall back to the
/// single TP-relative offset approach).
fn dtv_pointer_offset() -> Option<isize> {
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    return Some(8);

    #[cfg(all(target_arch = "aarch64", target_env = "gnu"))]
    return Some(0);

    #[cfg(all(target_arch = "x86_64", target_env = "musl"))]
    return Some(8);

    #[cfg(all(target_arch = "aarch64", target_env = "musl"))]
    return Some(-8);

    #[allow(unreachable_code)]
    None
}

/// Resolve the DTV pointer offset and validate it against a known `(modid, tls_block_base)`
/// pair from `dl_iterate_phdr`.
///
/// Returns the validated signed offset, or `None` if the platform is unsupported or
/// validation fails (which would indicate a libc ABI change).
pub(crate) fn validated_dtv_offset(tp: u64, modid: usize, tls_block_base: usize) -> Option<isize> {
    let offset = dtv_pointer_offset()?;
    let dtv_addr = tp.checked_add_signed(offset as i64)?;

    // Validate: read the DTV pointer, then check dtv[modid] == tls_block_base.
    #[cfg(target_env = "gnu")]
    const DTV_ENTRY_SIZE: u64 = 16;
    #[cfg(target_env = "musl")]
    const DTV_ENTRY_SIZE: u64 = 8;

    // SAFETY: dtv_addr is within the calling thread's own TCB, which is valid and
    // readable memory. The offset is a well-known libc ABI constant.
    let dtv_ptr = unsafe { *(dtv_addr as *const u64) };
    if dtv_ptr == 0 {
        return None;
    }

    let entry_addr = dtv_ptr.checked_add((modid as u64).checked_mul(DTV_ENTRY_SIZE)?)?;
    // SAFETY: if dtv_ptr is the real DTV and modid is valid (from dl_iterate_phdr),
    // this entry is within the allocated DTV array.
    let entry_value = unsafe { *(entry_addr as *const u64) };

    if entry_value as usize == tls_block_base {
        Some(offset)
    } else {
        None // ABI mismatch -- offset didn't lead to the expected DTV entry.
    }
}

/// Decode a record (at least 28 bytes) into an `OtelThreadContext`.
///
/// The input may be exactly 28 bytes (header-only) or up to `28 + attrs_data_size` bytes.
/// Attribute parsing is bounded by the smaller of `attrs_data_size` and the slice length.
pub(crate) fn decode_record(tid: i32, data: &[u8]) -> Option<OtelThreadContext> {
    if data.len() < RECORD_HEADER_SIZE {
        return None;
    }
    if data[24] != 1 {
        return None;
    }

    let trace_id = bytes_to_hex(&data[0..16]);
    let span_id = bytes_to_hex(&data[16..24]);
    let trace_flags = data[25];
    let attrs_data_size = u16::from_ne_bytes([data[26], data[27]]) as usize;
    let attrs_end = RECORD_HEADER_SIZE + attrs_data_size.min(data.len() - RECORD_HEADER_SIZE);
    let attributes = decode_attributes(&data[RECORD_HEADER_SIZE..attrs_end]);

    Some(OtelThreadContext {
        tid,
        thread_name: None,
        trace_id,
        span_id,
        trace_flags,
        attributes,
    })
}

/// Decode packed key-value attribute records.
///
/// Each entry: 1-byte key_index + 1-byte val_len + val_len bytes of value.
fn decode_attributes(data: &[u8]) -> Vec<OtelAttribute> {
    let mut attrs = Vec::new();
    let mut offset = 0;

    while offset + 2 <= data.len() {
        let key_index = data[offset];
        let val_len = data[offset + 1] as usize;
        offset += 2;

        if offset + val_len > data.len() {
            break;
        }

        let value = String::from_utf8_lossy(&data[offset..offset + val_len]).into_owned();
        attrs.push(OtelAttribute { key_index, value });
        offset += val_len;
    }

    attrs
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xF) as usize] as char);
    }
    s
}

/// Read a thread's name from `/proc/<pid>/task/<tid>/comm`.
pub(crate) fn read_thread_name(pid: u32, tid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        trace_flags: u8,
        attrs: &[u8],
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(RECORD_HEADER_SIZE + attrs.len());
        buf.extend_from_slice(trace_id);
        buf.extend_from_slice(span_id);
        buf.push(1); // valid
        buf.push(trace_flags);
        buf.extend_from_slice(&(attrs.len() as u16).to_ne_bytes());
        buf.extend_from_slice(attrs);
        buf
    }

    #[test]
    fn decode_valid_record() {
        let trace_id: [u8; 16] = core::array::from_fn(|i| (i + 1) as u8);
        let span_id: [u8; 8] = core::array::from_fn(|i| (0x11 + i) as u8);
        let attrs = [
            0u8, 16, b'a', b'b', b'c', b'd', b'e', b'f', b'0', b'1', b'2', b'3', b'4', b'5', b'6',
            b'7', b'8', b'9',
        ];

        let data = make_record(&trace_id, &span_id, 0x01, &attrs);
        let ctx = decode_record(42, &data).expect("should be Some");

        assert_eq!(ctx.tid, 42);
        assert_eq!(ctx.trace_id, "0102030405060708090a0b0c0d0e0f10");
        assert_eq!(ctx.span_id, "1112131415161718");
        assert_eq!(ctx.trace_flags, 0x01);
        assert_eq!(ctx.attributes.len(), 1);
        assert_eq!(ctx.attributes[0].key_index, 0);
        assert_eq!(ctx.attributes[0].value, "abcdef0123456789");
    }

    #[test]
    fn decode_header_only_record() {
        let data = make_record(&[0xAA; 16], &[0xBB; 8], 0x00, &[]);
        let ctx = decode_record(1, &data).expect("should be Some");
        assert_eq!(ctx.trace_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(ctx.attributes.is_empty());
    }

    #[test]
    fn decode_invalid_record_returns_none() {
        let mut data = make_record(&[0; 16], &[0; 8], 0, &[]);
        data[24] = 0; // valid = 0 (mid-update)
        assert!(decode_record(1, &data).is_none());
    }

    #[test]
    fn decode_too_short_returns_none() {
        assert!(decode_record(1, &[0u8; 10]).is_none());
    }

    #[test]
    fn decode_attributes_empty() {
        assert!(decode_attributes(&[]).is_empty());
    }

    #[test]
    fn decode_attributes_multiple() {
        let data = [
            0, 3, b'G', b'E', b'T', // key=0, val="GET"
            1, 4, b't', b'e', b's', b't', // key=1, val="test"
        ];
        let attrs = decode_attributes(&data);
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].key_index, 0);
        assert_eq!(attrs[0].value, "GET");
        assert_eq!(attrs[1].key_index, 1);
        assert_eq!(attrs[1].value, "test");
    }

    #[test]
    fn decode_attributes_truncated() {
        let data = [0, 10, b'a', b'b', b'c'];
        assert!(decode_attributes(&data).is_empty());
    }

    #[test]
    fn bytes_to_hex_works() {
        assert_eq!(bytes_to_hex(&[0x00, 0xff, 0xab, 0xcd]), "00ffabcd");
        assert_eq!(bytes_to_hex(&[]), "");
    }
}
