// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Value, WireType};
use std::io::{self, Write};

/// Encodes a [`varint`] according to protobuf semantics.
///
/// Serialization happens one byte at a time; use a buffered writer.
///
/// [`varint`]: https://protobuf.dev/programming-guides/encoding/#varints
#[inline]
pub(crate) fn encode(mut value: u64, writer: &mut impl Write) -> io::Result<()> {
    loop {
        let byte = if value < 0x80 {
            value as u8
        } else {
            ((value & 0x7F) | 0x80) as u8
        };
        writer.write_all(&[byte])?;
        if value < 0x80 {
            return Ok(());
        }
        value >>= 7;
    }
}

/// Returns the number of bytes it takes to varint encode the number, between
/// 1 and 10 bytes (inclusive).
#[inline]
pub(crate) fn proto_len(val: u64) -> u64 {
    // https://github.com/google/protobuf/blob/3.3.x/src/google/protobuf/io/coded_stream.h#L1301-L1309
    ((((val | 1).leading_zeros() ^ 63) * 9 + 73) / 64) as u64
}

unsafe impl Value for u64 {
    const WIRE_TYPE: WireType = WireType::Varint;

    #[inline]
    fn proto_len(&self) -> u64 {
        proto_len(*self)
    }

    #[inline]
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        encode(*self, writer)
    }
}

unsafe impl Value for i64 {
    const WIRE_TYPE: WireType = WireType::Varint;

    fn proto_len(&self) -> u64 {
        proto_len(*self as u64)
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        encode(*self as u64, writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_range() {
        assert_eq!(0u64.proto_len(), 1);
        assert_eq!(0x80u64.proto_len(), 2);
        assert_eq!(u64::MAX.proto_len(), 10);
    }
}
