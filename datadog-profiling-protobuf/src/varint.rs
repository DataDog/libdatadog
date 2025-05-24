// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{StringOffset, Value, WireType};
use std::io::{self, Write};

/// You can use varint to store any of the listed data types:
/// int32 | int64 | uint32 | uint64 | bool | enum | sint32 | sint64
#[repr(transparent)]
#[derive(Copy, Clone)]
pub struct Varint(pub u64);

impl Value for Varint {
    const WIRE_TYPE: WireType = WireType::Varint;

    /// Returns the number of bytes it takes to encode a varint. This is
    /// between 1 and 10 bytes, inclusive.
    fn proto_len(&self) -> u64 {
        // https://github.com/google/protobuf/blob/3.3.x/src/google/protobuf/io/coded_stream.h#L1301-L1309
        ((((self.0 | 1).leading_zeros() ^ 63) * 9 + 73) / 64) as u64
    }

    /// Encodes a [`varint`] according to protobuf semantics.
    ///
    /// Serialization happens one byte at a time; use a buffered writer.
    ///
    /// [`varint`]: https://protobuf.dev/programming-guides/encoding/#varints
    #[inline]
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut value = self.0;
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
}

impl From<u64> for Varint {
    fn from(value: u64) -> Self {
        Varint(value)
    }
}

impl From<&u64> for Varint {
    fn from(value: &u64) -> Self {
        Varint(*value)
    }
}

impl From<i64> for Varint {
    fn from(value: i64) -> Self {
        Varint(value as u64)
    }
}

impl From<&i64> for Varint {
    fn from(value: &i64) -> Self {
        Varint(*value as u64)
    }
}

impl From<StringOffset> for Varint {
    fn from(string_offset: StringOffset) -> Self {
        Self(u64::from(string_offset))
    }
}

impl From<&StringOffset> for Varint {
    fn from(string_offset: &StringOffset) -> Self {
        Self(u64::from(string_offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_range() {
        assert_eq!(Varint(0).proto_len(), 1);
        assert_eq!(Varint(0x80).proto_len(), 2);
        assert_eq!(Varint(u64::MAX).proto_len(), 10);
    }
}
