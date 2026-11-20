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

        let attrs_data_size = usize::from(u16::from_ne_bytes([buf[26], buf[27]]));
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
        let val_len = usize::from(data[offset + 1]);
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

// ── Pre-cached TLS metadata (populated at init, read at crash time) ─────────

/// Global cache for TLS symbol metadata, populated once by [`cache_otel_tls_info`]
/// at crashtracker init time (outside signal context). The collector reads this at
/// crash time via [`cached_tls_info`].
static CACHED_TLS_INFO: std::sync::OnceLock<TlsSymbolInfo> = std::sync::OnceLock::new();

/// Resolve and cache the `otel_thread_ctx_v1` TLS symbol metadata via `dl_iterate_phdr`.
///
/// **Must be called outside signal/crash context** (e.g. during `crashtracker::init`).
/// `dl_iterate_phdr` takes the loader lock, which is safe here but would deadlock
/// if called from a post-fork collector while another thread holds the lock.
///
/// Only the first call performs discovery; subsequent calls are no-ops.
pub(crate) fn cache_otel_tls_info() {
    CACHED_TLS_INFO.get_or_init(|| {
        resolve_tls_symbol_via_dl_iterate_phdr().unwrap_or(TlsSymbolInfo {
            symbol_offset: 0,
            tls_memsz: 0,
            tls_align: 0,
        })
    });
}

/// Read the cached TLS symbol info. Returns `None` if [`cache_otel_tls_info`] was
/// never called or the symbol wasn't found at init time.
pub(crate) fn cached_tls_info() -> Option<&'static TlsSymbolInfo> {
    CACHED_TLS_INFO.get().filter(|info| info.tls_memsz > 0) // sentinel: memsz==0 means "not found"
}

/// Discover `otel_thread_ctx_v1` TLS metadata via `dl_iterate_phdr`.
///
/// Only safe to call outside signal context (the loader lock is not async-signal-safe).
///
/// Phase 1: `dl_iterate_phdr` enumerates loaded objects with PT_TLS and collects their
/// file paths and TLS segment metadata.
/// Phase 2: For each candidate, goblin parses the ELF file from disk to search for the
/// symbol in `.dynsym`. This avoids manual pointer arithmetic on in-memory ELF structures.
fn resolve_tls_symbol_via_dl_iterate_phdr() -> Option<TlsSymbolInfo> {
    use std::path::PathBuf;

    struct Candidate {
        path: PathBuf,
        tls_memsz: usize,
        tls_align: usize,
    }

    struct State {
        candidates: Vec<Candidate>,
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
            unsafe { core::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) };

        // Only consider objects with a TLS segment.
        let Some(pt_tls) = phdrs.iter().find(|p| p.p_type == libc::PT_TLS) else {
            return 0;
        };

        // SAFETY: dlpi_name is a valid C string under the loader lock.
        let name_cstr = unsafe { core::ffi::CStr::from_ptr(info.dlpi_name) };
        let path = if name_cstr.to_bytes().is_empty() {
            // Main executable — dlpi_name is empty; use /proc/self/exe.
            PathBuf::from("/proc/self/exe")
        } else {
            PathBuf::from(name_cstr.to_string_lossy().as_ref())
        };

        state.candidates.push(Candidate {
            path,
            tls_memsz: pt_tls.p_memsz as usize,
            tls_align: pt_tls.p_align as usize,
        });

        0 // continue — collect all candidates
    }

    let mut state = State {
        candidates: Vec::new(),
    };

    // SAFETY: `callback` has the correct signature. `state` is live for the duration.
    unsafe {
        libc::dl_iterate_phdr(
            Some(callback),
            &mut state as *mut State as *mut core::ffi::c_void,
        );
    }

    // Phase 2: search each candidate's ELF file for the symbol using goblin.
    for candidate in &state.candidates {
        if let Some(symbol_offset) = find_tls_symbol_in_file(&candidate.path) {
            return Some(TlsSymbolInfo {
                symbol_offset,
                tls_memsz: candidate.tls_memsz,
                tls_align: candidate.tls_align,
            });
        }
    }

    None
}

/// Parse an ELF file from disk and search its `.dynsym` for `otel_thread_ctx_v1`.
/// Returns the symbol's `st_value` (TLS offset within the module's block).
fn find_tls_symbol_in_file(path: &std::path::Path) -> Option<usize> {
    let data = std::fs::read(path).ok()?;
    let elf = goblin::elf::Elf::parse(&data).ok()?;

    for sym in elf.dynsyms.iter() {
        if sym.st_type() != goblin::elf::sym::STT_TLS {
            continue;
        }
        if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
            continue;
        }
        if elf.dynstrtab.get_at(sym.st_name).map(str::as_bytes) == Some(SYMBOL_NAME) {
            return Some(sym.st_value as usize);
        }
    }
    None
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
/// Only needed when the DTV entry is `TLS_DTV_UNALLOCATED` (old glibc / old musl).
/// Modern runtimes populate the DTV and never reach this path.
///
/// - **x86_64** (variant II, glibc + musl): TLS grows downward: `TP - align_up(memsz, align)`
///
/// - **aarch64 glibc** (variant I): TLS grows upward: `align_up(TP + 16, align)` 16 = glibc TCB
///   size (`tcbhead_t`: 2 pointers — dtv + private). Ref: glibc `sysdeps/aarch64/nptl/tls.h`
///   (`TLS_TCB_SIZE`)
///
/// - **aarch64 musl** (variant I): TLS grows upward: `align_up(TP, align)` musl defines
///   `TLS_ABOVE_TP` for aarch64 and adjusts TP via `TP_ADJ(p)` = `(char*)(p) + sizeof(struct
///   __pthread)`, so TP already points past the TCB. Ref: musl `src/internal/pthread_impl.h`
///   (`TP_ADJ`, `TLS_ABOVE_TP`) Ref: musl `arch/aarch64/pthread_arch.h`
fn compute_static_tls_base(tp: u64, tls_memsz: usize, tls_align: usize) -> Option<u64> {
    if tls_memsz == 0 {
        return None;
    }
    let align = if tls_align == 0 { 1 } else { tls_align };

    #[cfg(target_arch = "x86_64")]
    {
        let aligned_size = (tls_memsz + align - 1) & !(align - 1);
        return Some(tp.wrapping_sub(aligned_size as u64));
    }
    #[cfg(all(target_arch = "aarch64", target_env = "gnu"))]
    {
        // glibc: TCB (16 bytes) sits between TP and TLS data.
        let tls_start = tp + 16;
        return Some(((tls_start as usize + align - 1) & !(align - 1)) as u64);
    }
    #[cfg(all(target_arch = "aarch64", target_env = "musl"))]
    {
        // musl: TP_ADJ already points past the pthread struct; TLS starts at TP.
        return Some(((tp as usize + align - 1) & !(align - 1)) as u64);
    }
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
