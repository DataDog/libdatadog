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
    /// # DTV layout
    ///
    /// On both glibc and musl the DTV pointer is at `TP + 8` (second TCB field).
    ///
    /// - **glibc**: 16-byte `dtv_t` entries. `dtv[0].counter` = generation;
    ///   `dtv[modid].pointer.val` = TLS block base. `TLS_DTV_UNALLOCATED = (void*)-1`.
    /// - **musl**: 8-byte `uintptr_t` entries. `dtv[modid]` = TLS block base.
    pub fn resolve_tls_addr(
        &mut self,
        tp: u64,
        modid: usize,
        offset: usize,
    ) -> Result<u64, OtelTlsError> {
        let dtv_ptr = self.read_u64(tp + 8)?;
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

/// Discover the TLS module ID and intra-module byte offset for `otel_thread_ctx_v1` by
/// walking loaded objects via `dl_iterate_phdr`.
///
/// Returns `None` if `tls_slot_address()` is 0 (symbol not linked) or no module's TLS range
/// contains the slot address (can happen on older glibc where `_dl_tls_get_addr_soft` returns
/// NULL after fork).
pub(crate) fn tls_module_and_offset() -> Option<(usize, usize)> {
    let slot_addr = libdd_otel_thread_ctx::linux::tls_slot_address();
    if slot_addr == 0 {
        return None;
    }

    struct State {
        slot_addr: usize,
        result: Option<(usize, usize)>,
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

        let tls_data = info.dlpi_tls_data;
        if tls_data.is_null() {
            return 0; // continue
        }

        // SAFETY: `dl_iterate_phdr` holds the loader lock, so no concurrent dlopen/dlclose
        // can invalidate the program headers. `dlpi_phdr` points to a valid array of
        // `dlpi_phnum` entries for a currently-loaded ELF object.
        let phdrs =
            unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
        for phdr in phdrs {
            if phdr.p_type == libc::PT_TLS {
                let base = tls_data as usize;
                let end = base + phdr.p_memsz as usize;
                if state.slot_addr >= base && state.slot_addr < end {
                    state.result = Some((info.dlpi_tls_modid, state.slot_addr - base));
                    return 1; // stop iteration
                }
            }
        }
        0 // continue
    }

    let mut state = State {
        slot_addr,
        result: None,
    };

    // SAFETY: `callback` has the correct signature for `dl_iterate_phdr`. `state` is live for
    // the duration of the call.
    unsafe {
        libc::dl_iterate_phdr(
            Some(callback),
            &mut state as *mut State as *mut core::ffi::c_void,
        );
    }

    state.result
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
