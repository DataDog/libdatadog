// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read an OTel `ThreadContextRecord` from the crashing process's memory.
//!
//! The collector (forked from the crashing process) emits the virtual address of
//! the record via the `DD_CRASHTRACK_*_OTEL_THREAD_CTX_PTR` protocol section.
//! This module reads the 640-byte record from `/proc/<pid>/mem` at that address
//! and decodes it into an [`OtelThreadContext`].

use alloc::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::crash_info::{OtelAttribute, OtelThreadContext};

const RECORD_SIZE: usize = 640;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum OtelTlsError {
    /// I/O error reading `/proc/<pid>/mem`.
    Io(std::io::Error),
}

impl fmt::Display for OtelTlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl From<std::io::Error> for OtelTlsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read and decode the `ThreadContextRecord` at `record_addr` in the target
/// process's address space.
///
/// Returns `Ok(None)` if the record's `valid` flag is not `1` (e.g. the
/// record was being updated at crash time).
pub(crate) fn read_otel_thread_context(
    pid: u32,
    tid: i32,
    record_addr: u64,
) -> Result<Option<OtelThreadContext>, OtelTlsError> {
    let mut record = [0u8; RECORD_SIZE];
    read_remote_memory(pid, record_addr, &mut record)?;

    let ctx = decode_thread_context_record(tid, &record);
    if let Some(mut ctx) = ctx {
        ctx.thread_name = read_thread_name(pid, tid);
        Ok(Some(ctx))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Remote memory reading
// ---------------------------------------------------------------------------

fn read_remote_memory(pid: u32, addr: u64, buf: &mut [u8]) -> Result<(), OtelTlsError> {
    let mut file = File::open(format!("/proc/{pid}/mem"))?;
    file.seek(SeekFrom::Start(addr))?;
    file.read_exact(buf)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Record decoding
// ---------------------------------------------------------------------------

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

pub(crate) fn read_thread_name(pid: u32, tid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
