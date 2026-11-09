// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read an OTel `ThreadContextRecord` from the crashing process's memory.

use alloc::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::crash_info::{OtelAttribute, OtelThreadContext};

const RECORD_SIZE: usize = 640;

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

/// Read the raw 640-byte record into `buf`. Returns `Ok(false)` if the record
/// is not valid (e.g. mid-update).
pub(crate) fn read_raw_record(
    pid: u32,
    record_addr: u64,
    buf: &mut [u8; RECORD_SIZE],
) -> Result<bool, OtelTlsError> {
    read_remote_memory(pid, record_addr, buf)?;
    Ok(buf[24] == 1) // valid flag
}

pub(crate) const OTEL_RECORD_SIZE: usize = RECORD_SIZE;

/// Decode a raw 640-byte record into an `OtelThreadContext` (no thread name).
pub(crate) fn decode_record(tid: i32, record: &[u8; RECORD_SIZE]) -> Option<OtelThreadContext> {
    decode_thread_context_record(tid, record)
}

fn read_remote_memory(pid: u32, addr: u64, buf: &mut [u8]) -> Result<(), OtelTlsError> {
    let mut file = File::open(format!("/proc/{pid}/mem"))?;
    file.seek(SeekFrom::Start(addr))?;
    file.read_exact(buf)?;
    Ok(())
}

/// Read the 8-byte TLS slot value (a record pointer) from `/proc/<pid>/mem`.
///
/// Returns `Ok(0)` if the slot is null (no context attached on that thread).
pub(crate) fn read_tls_slot_value(pid: u32, slot_addr: u64) -> Result<u64, OtelTlsError> {
    let mut buf = [0u8; 8];
    read_remote_memory(pid, slot_addr, &mut buf)?;
    Ok(u64::from_ne_bytes(buf))
}

/// Resolve the address of a TLS variable for a remote thread by walking its
/// Dynamic Thread Vector
///
/// This is correct for both static TLS (initial-exec / local-exec) and dynamic TLS
/// (global-dynamic / TLSDESC backed by `__tls_get_addr`)
///
/// # Arguments
///
/// * `pid`    – process ID (for `/proc/<pid>/mem` reads)
/// * `tp`     – thread pointer (`fs_base` on x86_64, `tpidr_el0` on aarch64)
/// * `modid`  – TLS module ID (from `dl_iterate_phdr`)
/// * `offset` – byte offset of the variable within the module's TLS block
///
/// # DTV layout
///
/// On both glibc and musl the DTV pointer lives at `TP + 8` (this is the second field of the TCB).
///
/// - **glibc**: each DTV entry is a 16-byte `dtv_t` union (`{ void *val; bool is_static; }`).
///   `dtv[0].counter` is the generation; `dtv[modid].pointer.val` is the TLS block base.
///   `TLS_DTV_UNALLOCATED = (void *)-1`.
///
/// - **musl**: each DTV entry is a plain `uintptr_t` (8 bytes). `dtv[0]` is unused/count;
///   `dtv[modid]` is the TLS block base.
pub(crate) fn resolve_tls_addr_via_dtv(
    pid: u32,
    tp: u64,
    modid: usize,
    offset: usize,
) -> Result<u64, OtelTlsError> {
    // The DTV pointer is at TP + 8 on both 64 bit glibc and musl.
    const DTV_PTR_OFFSET: u64 = 8;
    let mut buf = [0u8; 8];
    read_remote_memory(pid, tp + DTV_PTR_OFFSET, &mut buf)?;
    let dtv_ptr = u64::from_ne_bytes(buf);

    if dtv_ptr == 0 {
        return Err(OtelTlsError::DtvNull);
    }

    // glibc: sizeof(dtv_t) = 16 (union of {void*, bool} padded to 16 bytes).
    // musl:  sizeof(uintptr_t) = 8.
    #[cfg(target_env = "gnu")]
    const DTV_ENTRY_SIZE: u64 = 16;
    #[cfg(target_env = "musl")]
    const DTV_ENTRY_SIZE: u64 = 8;

    // modid fits in u64 on all 64-bit targets
    let entry_addr = dtv_ptr + (modid as u64) * DTV_ENTRY_SIZE;
    read_remote_memory(pid, entry_addr, &mut buf)?;
    let tls_block_base = u64::from_ne_bytes(buf);

    // TLS_DTV_UNALLOCATED = (void *)-1 on glibc; treat 0 as unallocated too.
    if tls_block_base == u64::MAX || tls_block_base == 0 {
        return Err(OtelTlsError::TlsNotAllocated);
    }

    Ok(tls_block_base + offset as u64)
}

/// Decode a raw 640-byte `ThreadContextRecord`.
///
/// Layout (from `libdd-otel-thread-ctx/src/linux.rs`):
/// - `[0..16]`   trace_id
/// - `[16..24]`  span_id
/// - `[24]`      valid (AtomicU8)
/// - `[25]`      trace_flags
/// - `[26..28]`  attrs_data_size (u16 native endian)
/// - `[28..640]` attrs_data (packed key-value records)
fn decode_thread_context_record(tid: i32, data: &[u8; RECORD_SIZE]) -> Option<OtelThreadContext> {
    let valid = data[24];
    if valid != 1 {
        return None;
    }

    let trace_id = bytes_to_hex(&data[0..16]);
    let span_id = bytes_to_hex(&data[16..24]);
    let trace_flags = data[25];
    let attrs_data_size = u16::from_ne_bytes([data[26], data[27]]) as usize;
    let attrs_end = 28 + attrs_data_size.min(612);
    let attributes = decode_attributes(&data[28..attrs_end]);

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

    #[test]
    fn decode_valid_record() {
        let mut data = [0u8; RECORD_SIZE];

        for (i, byte) in data[..16].iter_mut().enumerate() {
            *byte = (i + 1) as u8;
        }
        for (i, byte) in data[16..24].iter_mut().enumerate() {
            *byte = (0x11 + i) as u8;
        }
        data[24] = 1;
        data[25] = 0x01;

        let attr_data = &[
            0u8, 16, b'a', b'b', b'c', b'd', b'e', b'f', b'0', b'1', b'2', b'3', b'4', b'5', b'6',
            b'7', b'8', b'9',
        ];
        data[26..28].copy_from_slice(&(attr_data.len() as u16).to_ne_bytes());
        data[28..28 + attr_data.len()].copy_from_slice(attr_data);

        let ctx = decode_thread_context_record(42, &data).expect("should be Some");

        assert_eq!(ctx.tid, 42);
        assert_eq!(ctx.trace_id, "0102030405060708090a0b0c0d0e0f10");
        assert_eq!(ctx.span_id, "1112131415161718");
        assert_eq!(ctx.trace_flags, 0x01);
        assert_eq!(ctx.attributes.len(), 1);
        assert_eq!(ctx.attributes[0].key_index, 0);
        assert_eq!(ctx.attributes[0].value, "abcdef0123456789");
    }

    #[test]
    fn decode_invalid_record_returns_none() {
        let mut data = [0u8; RECORD_SIZE];
        data[24] = 0; // valid = 0 (mid-update)
        assert!(decode_thread_context_record(1, &data).is_none());
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
