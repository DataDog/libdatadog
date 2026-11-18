// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read an OTel `ThreadContextRecord` from a remote process via `/proc/<pid>/mem`.
//!
//! Also contains ELF-based TLS symbol discovery and DTV probing used by the
//! collector to locate `otel_thread_ctx_v1` without calling `dl_iterate_phdr`
//! (which can deadlock when the loader lock is held at crash time).

use alloc::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

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

// ── ELF-based TLS symbol discovery ──────────────────────────────────────────

const SYMBOL_NAME: &[u8] = b"otel_thread_ctx_v1";

/// TLS symbol info discovered from ELF files on disk.
pub(crate) struct TlsSymbolInfo {
    /// The symbol's `st_value` -- byte offset within the module's TLS block.
    pub symbol_offset: usize,
    /// PT_TLS `p_memsz` from the ELF program headers.
    pub tls_memsz: usize,
    /// PT_TLS `p_align` from the ELF program headers.
    pub tls_align: usize,
}

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
/// These are well-known, ABI-stable offsets for each (architecture, libc) combination.
/// See GDB's `get_tls_dtv_addr` for reference values.
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

/// Scan `/proc/<pid>/maps` and the corresponding ELF files to find the
/// `otel_thread_ctx_v1` TLS symbol.
pub(crate) fn discover_tls_symbol(pid: u32) -> Option<TlsSymbolInfo> {
    let maps_data = std::fs::read_to_string(format!("/proc/{pid}/maps")).ok()?;

    // Collect unique library paths from maps (deduplicate since libraries have multiple mappings).
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<(PathBuf, u64)> = Vec::new();

    for line in maps_data.lines() {
        // Format: "addr-addr perms offset dev inode   pathname"
        // Use split_whitespace to handle variable-width columns.
        let mut fields = line.split_whitespace();
        let Some(addr_range) = fields.next() else {
            continue;
        };
        // Skip perms, offset, dev, inode.
        if fields.nth(3).is_none() {
            continue;
        }
        let Some(path_str) = fields.next() else {
            continue;
        };
        if path_str.starts_with('[') {
            continue;
        }
        let path = Path::new(path_str);
        if !path.is_absolute() {
            continue;
        }
        if !seen.insert(path_str.to_string()) {
            continue;
        }
        // Parse the mapping start address.
        let mapping_start = addr_range
            .split('-')
            .next()
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);

        candidates.push((path.to_path_buf(), mapping_start));
    }

    for (path, _mapping_start) in candidates {
        if let Some(info) = find_tls_symbol_in_elf(&path) {
            return Some(info);
        }
    }

    None
}

/// Parse an ELF file on disk and search its dynamic symbol table for
/// `otel_thread_ctx_v1` (a `STT_TLS` symbol). Returns the symbol info including
/// PT_TLS metadata needed for static TLS fallback on old glibc.
fn find_tls_symbol_in_elf(path: &Path) -> Option<TlsSymbolInfo> {
    let data = std::fs::read(path).ok()?;
    let elf = goblin::elf::Elf::parse(&data).ok()?;

    let mut symbol_offset = None;
    for sym in elf.dynsyms.iter() {
        if sym.st_type() != goblin::elf::sym::STT_TLS {
            continue;
        }
        if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
            continue;
        }
        if let Some(name) = elf.dynstrtab.get_at(sym.st_name)
            && name.as_bytes() == SYMBOL_NAME
        {
            symbol_offset = Some(sym.st_value as usize);
            break;
        }
    }
    let symbol_offset = symbol_offset?;

    // Read PT_TLS memsz and align for computing static TLS addresses when the DTV
    // entry is TLS_DTV_UNALLOCATED (glibc 2.17 and older).
    let (tls_memsz, tls_align) = elf
        .program_headers
        .iter()
        .find(|ph| ph.p_type == goblin::elf::program_header::PT_TLS)
        .map(|ph| (ph.p_memsz as usize, ph.p_align as usize))
        .unwrap_or((0, 1));

    Some(TlsSymbolInfo {
        symbol_offset,
        tls_memsz,
        tls_align,
    })
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
/// - **Populated entry**: check if `dtv[i] + symbol_offset` -> valid record pointer.
/// - **`TLS_DTV_UNALLOCATED` (-1)**: compute the static TLS block address from TP and the PT_TLS
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
/// On glibc 2.17 and older, the DTV entry for static TLS modules is `TLS_DTV_UNALLOCATED`
/// (-1), but the block is always allocated inline with the TCB.
///
/// - **x86_64 (variant II)**: TLS grows downward: `TP - align_up(memsz, align)`
/// - **aarch64 (variant I)**: TLS grows upward: `align_up(TP + 16, align)`
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
