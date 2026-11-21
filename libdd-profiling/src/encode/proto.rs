// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A minimal protobuf writer: forward-only, into one fallible buffer.
//!
//! A nested message is written in place: its body first, then its length
//! prefix is inserted in front (by rotating the body right). That costs a
//! `memmove` of the body, and needs no temporary buffer and no second pass.

use crate::collections::alloc::Global;
use crate::collections::vec::TryVec;
use crate::encode::EncodeError;

const VARINT: u32 = 0;
#[cfg_attr(not(feature = "otel"), allow(dead_code, reason = "OTLP only"))]
const FIXED64: u32 = 1;
const LEN: u32 = 2;

/// Writes protobuf fields into a byte buffer of at most `limit` bytes.
pub struct Writer {
    buf: TryVec<u8, Global>,
    limit: usize,
}

impl Writer {
    pub const fn new(limit: usize) -> Self {
        Self {
            buf: TryVec::new_in(Global),
            limit,
        }
    }

    pub fn finish(self) -> TryVec<u8, Global> {
        self.buf
    }

    fn reserve(&mut self, additional: usize) -> Result<(), EncodeError> {
        let needed = self
            .buf
            .len()
            .checked_add(additional)
            .ok_or(EncodeError::TooLarge)?;
        if needed > self.limit {
            return Err(EncodeError::TooLarge);
        }
        self.buf
            .try_reserve(additional)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    /// Appends raw bytes.
    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.reserve(bytes.len())?;
        self.buf
            .try_extend_from_slice(bytes)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    fn varint(&mut self, value: u64) -> Result<(), EncodeError> {
        let (bytes, len) = varint_bytes(value);
        self.raw(bytes.get(..len).unwrap_or(&[]))
    }

    fn tag(&mut self, field: u32, wire: u32) -> Result<(), EncodeError> {
        self.varint(u64::from(field << 3 | wire))
    }

    /// A `uint64`/`uint32`/`int32`(non-negative) field; zero is omitted.
    pub fn uint64(&mut self, field: u32, value: u64) -> Result<(), EncodeError> {
        if value == 0 {
            return Ok(());
        }
        self.tag(field, VARINT)?;
        self.varint(value)
    }

    /// An `int64` field (two's complement varint); zero is omitted.
    pub fn int64(&mut self, field: u32, value: i64) -> Result<(), EncodeError> {
        self.uint64(field, value.cast_unsigned())
    }

    /// A `fixed64` field; zero is omitted.
    #[cfg_attr(not(feature = "otel"), allow(dead_code, reason = "OTLP only"))]
    pub fn fixed64(&mut self, field: u32, value: u64) -> Result<(), EncodeError> {
        if value == 0 {
            return Ok(());
        }
        self.tag(field, FIXED64)?;
        self.raw(&value.to_le_bytes())
    }

    /// A `bytes` or `string` field, written even when empty (repeated
    /// string tables need their empty entries).
    pub fn bytes(&mut self, field: u32, value: &[u8]) -> Result<(), EncodeError> {
        self.tag(field, LEN)?;
        self.varint(value.len() as u64)?;
        self.raw(value)
    }

    /// A `string` field; empty is omitted.
    #[cfg_attr(not(feature = "otel"), allow(dead_code, reason = "OTLP only"))]
    pub fn string(&mut self, field: u32, value: &str) -> Result<(), EncodeError> {
        if value.is_empty() {
            return Ok(());
        }
        self.bytes(field, value.as_bytes())
    }

    /// A packed repeated varint field from `values`; empty is omitted.
    pub fn packed_varints(
        &mut self,
        field: u32,
        values: impl Iterator<Item = u64> + Clone,
    ) -> Result<(), EncodeError> {
        let len: usize = values.clone().map(|v| varint_bytes(v).1).sum();
        if len == 0 {
            return Ok(());
        }
        self.tag(field, LEN)?;
        self.varint(len as u64)?;
        for value in values {
            self.varint(value)?;
        }
        Ok(())
    }

    /// A packed repeated `fixed64` field; empty is omitted.
    #[cfg_attr(not(feature = "otel"), allow(dead_code, reason = "OTLP only"))]
    pub fn packed_fixed64(&mut self, field: u32, values: &[u64]) -> Result<(), EncodeError> {
        if values.is_empty() {
            return Ok(());
        }
        self.tag(field, LEN)?;
        self.varint((values.len() * 8) as u64)?;
        for value in values {
            self.raw(&value.to_le_bytes())?;
        }
        Ok(())
    }

    /// A nested message field, whose body `body` writes. Written even when
    /// empty, since repeated messages and zero table entries need it.
    pub fn message(
        &mut self,
        field: u32,
        body: impl FnOnce(&mut Self) -> Result<(), EncodeError>,
    ) -> Result<(), EncodeError> {
        self.tag(field, LEN)?;
        let start = self.buf.len();
        body(self)?;
        let body_len = self.buf.len() - start;
        let (prefix, prefix_len) = varint_bytes(body_len as u64);
        let prefix = prefix.get(..prefix_len).unwrap_or(&[]);
        // Append the prefix, then rotate it to the front of the body:
        // reversing `body ++ prefix` gives `rev(prefix) ++ rev(body)`, and
        // reversing each part restores them, prefix first. (`rotate_right`
        // would do the same, but keeps a length assert out of line.)
        self.raw(prefix)?;
        if let Some(tail) = self.buf.get_mut(start..) {
            tail.reverse();
            if let Some((head, rest)) = tail.split_at_mut_checked(prefix_len) {
                head.reverse();
                rest.reverse();
            }
        }
        Ok(())
    }
}

/// `value` as a varint: the bytes and how many of them are used.
const fn varint_bytes(mut value: u64) -> ([u8; 10], usize) {
    let mut bytes = [0_u8; 10];
    let mut len = 0;
    while len < 10 {
        let [low, ..] = value.to_le_bytes();
        value >>= 7;
        if value == 0 {
            bytes[len] = low & 0x7f;
            len += 1;
            break;
        }
        bytes[len] = low | 0x80;
        len += 1;
    }
    (bytes, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_match_the_reference_encoding() {
        for (value, expected) in [
            (0_u64, &[0_u8][..]),
            (1, &[1]),
            (127, &[0x7f]),
            (128, &[0x80, 1]),
            (300, &[0xac, 2]),
            (
                u64::MAX,
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1],
            ),
        ] {
            let (bytes, len) = varint_bytes(value);
            assert_eq!(&bytes[..len], expected, "{value}");
        }
    }

    #[test]
    fn nested_messages_get_length_prefixes() {
        let mut w = Writer::new(1 << 20);
        w.message(1, |w| {
            w.uint64(1, 150)?;
            w.message(2, |w| w.string(1, "hi"))
        })
        .unwrap();
        // field 1 len, { field1 varint 150, field 2 len { field 1 "hi" } }
        assert_eq!(
            w.finish().as_slice(),
            &[
                0x0a, 0x09, 0x08, 0x96, 0x01, 0x12, 0x04, 0x0a, 0x02, b'h', b'i'
            ]
        );
    }

    #[test]
    fn a_long_body_gets_a_two_byte_prefix() {
        let mut w = Writer::new(1 << 20);
        let long = [b'x'; 200];
        w.message(3, |w| w.bytes(1, &long)).unwrap();
        let out = w.finish();
        assert_eq!(&out[..3], &[0x1a, 0xcb, 0x01]); // 203 = 0xcb 0x01
        assert_eq!(&out[3..6], &[0x0a, 0xc8, 0x01]); // field 1, 200 bytes
        assert_eq!(out.len(), 3 + 3 + 200);
    }

    #[test]
    fn the_limit_is_enforced() {
        let mut w = Writer::new(4);
        assert_eq!(w.bytes(1, b"12345"), Err(EncodeError::TooLarge));
    }
}
