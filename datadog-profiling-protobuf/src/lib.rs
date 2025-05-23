// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod function;
mod label;
mod location;
mod mapping;
mod sample;
mod string;
mod value_type;

#[cfg(feature = "prost_impls")]
pub mod prost_impls;

pub use function::*;
pub use label::*;
pub use location::*;
pub use mapping::*;
pub use sample::*;
pub use string::*;
pub use value_type::*;

use std::io::{self, Write};

/// A tag is a combination of a wire_type, stored in the least significant
/// three bits, and the field number that is defined in the .proto file.
#[derive(Copy, Clone)]
pub struct Tag {
    field: u32,
    wire_type: WireType,
}

/// A value is stored differently depending on the wire_type.
pub trait Value {
    const WIRE_TYPE: WireType;

    fn proto_len(&self) -> u64;

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()>;

    #[inline]
    fn field(self, field: u32) -> Pair<Self>
    where
        Self: Sized,
    {
        Pair::new(field, self)
    }
}

/// You can use varint to store any of the listed data types:
/// int32 | int64 | uint32 | uint64 | bool | enum | sint32 | sint64
#[repr(transparent)]
#[derive(Copy, Clone)]
pub struct Varint(pub u64);

pub struct Pair<V: Value> {
    field: u32,
    value: V,
}

impl<V: Value> Pair<V> {
    #[inline]
    pub const fn new(field: u32, value: V) -> Self {
        Pair { field, value }
    }

    pub fn proto_len(&self) -> u64 {
        let tag = Tag::new(self.field, V::WIRE_TYPE).proto_len();
        let value = self.value.proto_len();
        let len_prefix = if V::WIRE_TYPE == WireType::LengthDelimited {
            Varint(value).proto_len()
        } else {
            0
        };
        tag + len_prefix + value
    }

    #[inline]
    pub fn proto_len_small(&self) -> u64 {
        if self.value.proto_len() != 0 {
            self.proto_len()
        } else {
            0
        }
    }

    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        Tag::new(self.field, V::WIRE_TYPE).encode(writer)?;
        if V::WIRE_TYPE == WireType::LengthDelimited {
            let len = self.value.proto_len();
            Varint(len).encode(writer)?;
        }
        self.value.encode(writer)
    }

    #[inline]
    pub fn encode_small(&self, writer: &mut impl Write) -> io::Result<()> {
        let len = self.value.proto_len();
        if len == 0 {
            return Ok(());
        }

        Tag::new(self.field, V::WIRE_TYPE).encode(writer)?;
        if V::WIRE_TYPE == WireType::LengthDelimited {
            Varint(len).encode(writer)?;
        }
        self.value.encode(writer)
    }
}

pub trait TagEncodable {
    fn encode_with_tag<W: Write>(&self, w: &mut W, field: u32) -> io::Result<()>;
}

/// The smallest possible protobuf field number.
const MIN_FIELD: u32 = 1;

/// The largest possible protobuf field number.
const MAX_FIELD: u32 = (1 << 29) - 1;

/// An encoded 64-bit unsigned number takes between 1 and 10 bytes, inclusive.
pub const MAX_VARINT_LEN: u64 = 10;

/// Represents the wire type for in-wire protobuf. There are more types than
/// are represented here; these are just the supported ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}
impl Varint {
    /// Returns the number of bytes it takes to encode a varint. This is
    /// between 1 and 10 bytes, inclusive.
    pub const fn proto_len(&self) -> u64 {
        // https://github.com/google/protobuf/blob/3.3.x/src/google/protobuf/io/coded_stream.h#L1301-L1309
        ((((self.0 | 1).leading_zeros() ^ 63) * 9 + 73) / 64) as u64
    }
}

impl Value for Varint {
    const WIRE_TYPE: WireType = WireType::Varint;

    fn proto_len(&self) -> u64 {
        self.proto_len()
    }

    /// Encodes a varint according to protobuf semantics.
    ///
    /// Note that it will write between 1 and 10 bytes, inclusive. You should
    /// probably write to a buffered Write object--if you write directly to
    /// something like a TCP stream, it's going to send one byte at a time,
    /// which is excessively inefficient. In libdatadog, we typically write to
    /// some sort of compressor which has its own input buffer.
    ///
    /// See https://protobuf.dev/programming-guides/encoding/#varints
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

impl Tag {
    #[cfg_attr(debug_assertions, track_caller)]
    #[inline]
    pub const fn new(field: u32, wire_type: WireType) -> Self {
        debug_assert!(field >= MIN_FIELD && field <= MAX_FIELD);
        Self { field, wire_type }
    }

    #[inline]
    pub const fn proto_len(self) -> u64 {
        self.into_varint().proto_len()
    }

    #[inline]
    pub fn encode<W: Write>(self, writer: &mut W) -> io::Result<()> {
        self.into_varint().encode(writer)
    }

    #[inline]
    pub const fn into_varint(self) -> Varint {
        Varint(((self.field << 3) | self.wire_type as u32) as u64)
    }
}

pub struct Packed<'a, T: Into<Varint>> {
    values: &'a [T],
}

impl<'a, T: Into<Varint>> Packed<'a, T>
where
    Varint: From<&'a T>,
{
    pub fn new(values: &'a [T]) -> Self {
        Self { values }
    }
}

impl<'a, T: Into<Varint>> Value for Packed<'a, T>
where
    Varint: From<&'a T>,
{
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.values
            .iter()
            .map(|x| Varint::from(x).proto_len())
            .sum()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        for value in self.values {
            Varint::from(value).encode(writer)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_varint_len() {
        assert_eq!(MAX_VARINT_LEN, 10);
        assert_eq!(MAX_VARINT_LEN, Varint(u64::MAX).proto_len());
    }
}
