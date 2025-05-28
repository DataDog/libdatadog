// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{StringOffset, Value, Varint, WireType};
use std::io::{self, Write};

impl Value for u64 {
    const WIRE_TYPE: WireType = WireType::Varint;

    fn proto_len(&self) -> u64 {
        // https://github.com/google/protobuf/blob/3.3.x/src/google/protobuf/io/coded_stream.h#L1301-L1309
        ((((self | 1).leading_zeros() ^ 63) * 9 + 73) / 64) as u64
    }

    /// Encodes a [`varint`] according to protobuf semantics.
    ///
    /// Serialization happens one byte at a time; use a buffered writer.
    ///
    /// [`varint`]: https://protobuf.dev/programming-guides/encoding/#varints
    #[inline]
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut value = *self;
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

impl Value for i64 {
    const WIRE_TYPE: WireType = WireType::Varint;

    fn proto_len(&self) -> u64 {
        (*self as u64).proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        (*self as u64).encode(writer)
    }
}

unsafe impl Varint for u64 {}
unsafe impl Varint for i64 {}
unsafe impl Varint for StringOffset {}

impl<T: Varint> Value for &'_ [T] {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.iter().map(Value::proto_len).sum()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        for value in self.iter() {
            value.encode(writer)?;
        }
        Ok(())
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
