// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod function;
mod label;
mod location;
mod mapping;
mod sample;
mod string;
mod value_type;
mod varint;

#[cfg(feature = "prost_impls")]
pub mod prost_impls;

pub use function::*;
pub use label::*;
pub use location::*;
pub use mapping::*;
pub use sample::*;
pub use string::*;
pub use value_type::*;
pub use varint::*;

use std::io::{self, Write};

/// A tag is a combination of a wire_type, stored in the least significant
/// three bits, and the field number that is defined in the .proto file.
#[derive(Copy, Clone)]
pub struct Tag(u32);

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
        Pair { field, value: self }
    }
}

/// A tag and value pair.
///
/// The wire type isn't stored; it's provided by the Value implementation,
/// which allows us to specialize code.
pub struct Pair<V: Value> {
    field: u32,
    value: V,
}

impl<V: Value> Pair<V> {
    /// Calculate the size of pair, without using the zero-size optimization.
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

    /// Calculate the size of pair, using the zero-size optimization.
    #[inline]
    pub fn proto_len_small(&self) -> u64 {
        if self.value.proto_len() != 0 {
            self.proto_len()
        } else {
            0
        }
    }

    /// Encodes into protobuf, without using the zero-size optimization.
    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        Tag::new(self.field, V::WIRE_TYPE).encode(writer)?;
        if V::WIRE_TYPE == WireType::LengthDelimited {
            let len = self.value.proto_len();
            Varint(len).encode(writer)?;
        }
        self.value.encode(writer)
    }

    /// Encodes into protobuf, using the zero-size optimization.
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

/// The smallest possible protobuf field number.
const MIN_FIELD: u32 = 1;

/// The largest possible protobuf field number.
const MAX_FIELD: u32 = (1 << 29) - 1;

/// Represents the wire type for in-wire protobuf. There are more types than
/// are represented here; these are just the supported ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}

impl Tag {
    #[cfg_attr(debug_assertions, track_caller)]
    #[inline]
    pub const fn new(field: u32, wire_type: WireType) -> Self {
        debug_assert!(field >= MIN_FIELD && field <= MAX_FIELD);
        Self((field << 3) | wire_type as u32)
    }

    #[inline]
    pub fn proto_len(self) -> u64 {
        Varint(self.0 as u64).proto_len()
    }

    #[inline]
    pub fn encode<W: Write>(self, writer: &mut W) -> io::Result<()> {
        Varint(self.0 as u64).encode(writer)
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
