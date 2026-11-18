// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read an OTel `ThreadContextRecord` from a remote process via `/proc/<pid>/mem`
//! and ELF-based TLS symbol discovery and DTV probing

use crate::crash_info::{OtelAttribute, OtelThreadContext};
use alloc::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

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
    pub(crate) fn read_exact(&mut self, addr: u64, buf: &mut [u8]) -> Result<(), OtelTlsError> {
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

// ── TLS symbol info ─────────────────────────────────────────────────────────

const SYMBOL_NAME: &[u8] = b"otel_thread_ctx_v1";

/// TLS symbol info: everything the collector needs to resolve the OTel TLS variable.
pub(crate) struct TlsSymbolInfo {
    /// The symbol's `st_value` -- byte offset within the module's TLS block.
    pub symbol_offset: usize,
    /// PT_TLS `p_memsz` from the ELF program headers.
    pub tls_memsz: usize,
    /// PT_TLS `p_align` from the ELF program headers.
    pub tls_align: usize,
}

// ── Pre-cached TLS metadata ─────────

use core::sync::atomic::{AtomicPtr, Ordering::SeqCst};

/// Global cache for TLS symbol metadata, populated by [`cache_otel_tls_info`] at
/// init time. The collector reads this at crash time.
static CACHED_TLS_INFO: AtomicPtr<TlsSymbolInfo> = AtomicPtr::new(core::ptr::null_mut());

/// Resolve and cache the `otel_thread_ctx_v1` TLS symbol metadata via `dl_iterate_phdr`.
///
/// Must be called outside signal/crash context (e.g. during `crashtracker::init`).
pub(crate) fn cache_otel_tls_info() {
    let info = resolve_tls_symbol_via_dl_iterate_phdr();
    let new_ptr = match info {
        Some(i) => Box::into_raw(Box::new(i)),
        None => core::ptr::null_mut(),
    };
    let old = CACHED_TLS_INFO.swap(new_ptr, SeqCst);
    if !old.is_null() {
        // SAFETY: old came from Box::into_raw above.
        unsafe {
            drop(Box::from_raw(old));
        }
    }
}

/// Read the cached TLS symbol info. Returns `None` if [`cache_otel_tls_info`] was
/// never called or the symbol wasn't found.
///
/// The returned reference borrows the global and is valid until the next
/// `cache_otel_tls_info` call (which only happens at init, not concurrently with
/// crash handling).
pub(crate) fn cached_tls_info() -> Option<&'static TlsSymbolInfo> {
    let ptr = CACHED_TLS_INFO.load(SeqCst);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: the pointer came from Box::into_raw and is not freed until the next
        // cache_otel_tls_info call. The signal handler's one-time guard ensures no
        // concurrent init during crash handling.
        Some(unsafe { &*ptr })
    }
}

/// Discover `otel_thread_ctx_v1` TLS metadata via `dl_iterate_phdr`.
///
/// Only safe to call outside signal context (the loader lock is not async-signal-safe).
fn resolve_tls_symbol_via_dl_iterate_phdr() -> Option<TlsSymbolInfo> {
    struct State {
        result: Option<TlsSymbolInfo>,
    }

    unsafe extern "C" fn callback(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut core::ffi::c_void,
    ) -> core::ffi::c_int {
        // SAFETY: `data` points to the `State` local in the enclosing function.
        let state = unsafe { &mut *(data as *mut State) };
        // SAFETY: `info` is a valid `dl_phdr_info` provided by `dl_iterate_phdr`.
        let info = unsafe { &*info };

        // SAFETY: program headers are valid under the loader lock.
        let phdrs =
            unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };

        // SAFETY: phdrs and info.dlpi_addr are valid under the loader lock.
        if let Some(offset) = unsafe { find_tls_symbol_in_phdrs(phdrs, info.dlpi_addr) } {
            let (tls_memsz, tls_align) = phdrs
                .iter()
                .find(|p| p.p_type == libc::PT_TLS)
                .map(|p| (p.p_memsz as usize, p.p_align as usize))
                .unwrap_or((0, 1));

            state.result = Some(TlsSymbolInfo {
                symbol_offset: offset,
                tls_memsz,
                tls_align,
            });
            return 1; // stop iteration
        }

        0 // continue
    }

    let mut state = State { result: None };

    // SAFETY: `callback` has the correct signature. `state` is live for the duration.
    unsafe {
        libc::dl_iterate_phdr(
            Some(callback),
            &mut state as *mut State as *mut core::ffi::c_void,
        );
    }

    state.result
}

/// Search the in-memory dynamic symbol table of a loaded ELF object for
/// `otel_thread_ctx_v1`. Returns the symbol's `st_value` if found.
///
/// # Safety
///
/// `phdrs` must point to valid program headers of a currently-loaded ELF object.
/// Called from within a `dl_iterate_phdr` callback where the loader lock is held.
unsafe fn find_tls_symbol_in_phdrs(phdrs: &[libc::Elf64_Phdr], base_addr: u64) -> Option<usize> {
    // ELF constants.
    const DT_NULL: i64 = 0;
    const DT_STRTAB: i64 = 5;
    const DT_SYMTAB: i64 = 6;
    const DT_GNU_HASH: i64 = 0x6fff_fef5_u64 as i64;
    const STT_TLS: u8 = 6;
    const SHN_UNDEF: u16 = 0;

    #[cfg(target_env = "musl")]
    let resolve = |d_val: u64| -> usize { (base_addr + d_val) as usize };
    #[cfg(not(target_env = "musl"))]
    let resolve = |d_val: u64| -> usize { d_val as usize };

    // Skip objects without a TLS segment (like vDSO) have unrelocated d_val entries that would
    // crash us.
    if !phdrs.iter().any(|p| p.p_type == libc::PT_TLS) {
        return None;
    }

    // Find PT_DYNAMIC.
    let dyn_phdr = phdrs.iter().find(|p| p.p_type == libc::PT_DYNAMIC)?;
    let dyn_addr = base_addr + dyn_phdr.p_vaddr;

    // Walk dynamic entries.
    let mut symtab: *const libc::Elf64_Sym = core::ptr::null();
    let mut strtab: *const u8 = core::ptr::null();
    let mut gnu_hash_ptr: *const u32 = core::ptr::null();

    // libc doesn't expose Elf64_Dyn
    #[repr(C)]
    struct Elf64Dyn {
        d_tag: i64,
        d_val: u64,
    }

    let mut dyn_ptr = dyn_addr as *const Elf64Dyn;
    // SAFETY: dynamic section is DT_NULL-terminated, within mapped pages.
    unsafe {
        loop {
            let entry = &*dyn_ptr;
            if entry.d_tag == DT_NULL {
                break;
            }
            match entry.d_tag {
                DT_SYMTAB => symtab = resolve(entry.d_val) as *const libc::Elf64_Sym,
                DT_STRTAB => strtab = resolve(entry.d_val) as *const u8,
                DT_GNU_HASH => gnu_hash_ptr = resolve(entry.d_val) as *const u32,
                _ => {}
            }
            dyn_ptr = dyn_ptr.add(1);
        }
    }

    if symtab.is_null() || strtab.is_null() || gnu_hash_ptr.is_null() {
        return None;
    }

    // GNU hash lookup for SYMBOL_NAME.
    // SAFETY: all pointers are within mapped pages under the loader lock.
    unsafe {
        let nbuckets = *gnu_hash_ptr;
        let symoffset = *gnu_hash_ptr.add(1);
        let bloom_size = *gnu_hash_ptr.add(2);

        if nbuckets == 0 {
            return None;
        }

        let h = gnu_hash_symbol_name();
        let buckets = gnu_hash_ptr.add(4 + bloom_size as usize * 2);
        let bucket_idx = h % nbuckets;
        let mut sym_idx = *buckets.add(bucket_idx as usize);
        if sym_idx == 0 {
            return None;
        }

        let chains = buckets.add(nbuckets as usize);
        loop {
            if sym_idx < symoffset {
                return None;
            }
            let chain_val = *chains.add((sym_idx - symoffset) as usize);
            if (chain_val ^ h) & !1 == 0 {
                let sym = &*symtab.add(sym_idx as usize);
                if sym.st_info & 0xf == STT_TLS
                    && sym.st_shndx != SHN_UNDEF
                    && streq(strtab.add(sym.st_name as usize), SYMBOL_NAME)
                {
                    return Some(sym.st_value as usize);
                }
            }
            if chain_val & 1 != 0 {
                break;
            }
            sym_idx += 1;
        }
    }

    None
}

/// GNU hash of `SYMBOL_NAME` (compile-time constant).
const fn gnu_hash_symbol_name() -> u32 {
    let mut h: u32 = 5381;
    let mut i = 0;
    while i < SYMBOL_NAME.len() {
        h = h.wrapping_mul(33).wrapping_add(SYMBOL_NAME[i] as u32);
        i += 1;
    }
    h
}

/// Compare a NUL-terminated C string at `ptr` with `name`.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string.
unsafe fn streq(ptr: *const u8, name: &[u8]) -> bool {
    unsafe { name.iter().enumerate().all(|(j, &b)| *ptr.add(j) == b) && *ptr.add(name.len()) == 0 }
}

// ── DTV probing ─────────────────────────────────────────────────────────────

/// Result of DTV probing: how to resolve TLS addresses for all threads.
pub(crate) enum TlsResolution {
    /// DTV entry was populated; use the standard DTV walk for all threads.
    Dtv { module_id: usize },
    /// DTV entry was `TLS_DTV_UNALLOCATED` (-1); the TLS block is at a fixed offset
    /// from TP (static TLS, common on glibc 2.17). Use TP-relative addressing.
    StaticOffset { tp_to_slot: i64 },
}

/// Signed byte offset from the thread pointer (TP) to the DTV pointer.
///
/// These are well-known, ABI-stable offsets for each combination.
/// GDB references:
/// - x86_64: <https://sourceware.org/git/?p=binutils-gdb.git;a=blob;f=gdb/amd64-linux-tdep.c>
///   (`amd64_linux_get_tls_dtv_addr`)
/// - aarch64: <https://sourceware.org/git/?p=binutils-gdb.git;a=blob;f=gdb/aarch64-linux-tdep.c>
///   (`aarch64_linux_get_tls_dtv_addr`)
pub(crate) fn dtv_pointer_offset() -> Option<isize> {
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

/// Read a `u64` from a remote process's memory via `/proc/<pid>/mem`.
fn remote_read_u64(mem: &mut ProcMemReader, addr: u64) -> Option<u64> {
    let mut buf = [0u8; 8];
    mem.read_exact(addr, &mut buf).ok()?;
    Some(u64::from_ne_bytes(buf))
}

/// Discover how to resolve the OTel TLS variable by probing DTV entries on a stopped thread.
///
/// Instead of walking glibc's internal `link_map` chain (whose `l_tls_modid` offset varies
/// across glibc versions), this probes DTV entries 1..N:
///
/// - Populated entry: check if `dtv[i] + symbol_offset` -> valid record pointer.
/// - `TLS_DTV_UNALLOCATED` (-1): compute the static TLS block address from TP and the PT_TLS
///   segment size, then validate. This is the path glibc 2.17 (CentOS 7) takes.
pub(crate) fn probe_dtv_for_resolution(
    mem: &mut ProcMemReader,
    tp: u64,
    dtv_offset: isize,
    symbol: &TlsSymbolInfo,
) -> Option<TlsResolution> {
    #[cfg(target_env = "gnu")]
    const DTV_ENTRY_SIZE: u64 = 16;
    #[cfg(target_env = "musl")]
    const DTV_ENTRY_SIZE: u64 = 8;

    const MAX_PROBE: usize = 64;

    let dtv_addr = tp.checked_add_signed(dtv_offset as i64)?;
    let dtv_ptr = remote_read_u64(mem, dtv_addr)?;
    if dtv_ptr == 0 {
        return None;
    }

    for modid in 1..=MAX_PROBE {
        let entry_val =
            remote_read_u64(mem, dtv_ptr + (modid as u64) * DTV_ENTRY_SIZE).unwrap_or(0);

        if entry_val == 0 {
            continue;
        }

        let tls_block_base = if entry_val == u64::MAX {
            // TLS_DTV_UNALLOCATED: the DTV entry hasn't been lazily filled, but for
            // static TLS the block is always allocated next to the TCB. Compute its
            // address from TP and the PT_TLS block size.
            compute_static_tls_base(tp, symbol.tls_memsz, symbol.tls_align)?
        } else {
            entry_val
        };

        // Check if this TLS block + symbol_offset -> non-null slot -> valid record.
        let slot_addr = tls_block_base + symbol.symbol_offset as u64;
        let slot_val = remote_read_u64(mem, slot_addr).unwrap_or(0);
        if slot_val == 0 {
            continue;
        }
        let mut valid_byte = [0u8];
        if mem.read_exact(slot_val + 24, &mut valid_byte).is_err() || valid_byte[0] != 1 {
            continue;
        }

        // Found the right module.
        return if entry_val == u64::MAX {
            // Static TLS: record the TP-relative offset for use on all threads.
            let tp_to_slot = slot_addr as i64 - tp as i64;
            Some(TlsResolution::StaticOffset { tp_to_slot })
        } else {
            Some(TlsResolution::Dtv { module_id: modid })
        };
    }

    None
}

/// Compute the base address of a static TLS block from the thread pointer.
///
/// - x86_64 (variant II): TLS grows downward: `TP - align_up(memsz, align)`
/// - aarch64 (variant I): TLS grows upward: `align_up(TP + 16, align)`
fn compute_static_tls_base(tp: u64, tls_memsz: usize, tls_align: usize) -> Option<u64> {
    if tls_memsz == 0 {
        return None;
    }
    let align = if tls_align == 0 { 1 } else { tls_align };
    let aligned_size = (tls_memsz + align - 1) & !(align - 1);

    #[cfg(target_arch = "x86_64")]
    return Some(tp.wrapping_sub(aligned_size as u64));
    #[cfg(target_arch = "aarch64")]
    return Some({
        // TCB is 16 bytes on aarch64 glibc (2 pointers: dtv + private).
        let after_tcb = tp + 16;
        ((after_tcb as usize + align - 1) & !(align - 1)) as u64
    });
    #[allow(unreachable_code)]
    None
}

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
