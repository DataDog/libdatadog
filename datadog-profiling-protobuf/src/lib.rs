// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! This crate implements Protobuf serializers for [`profiles`], including:
//!
//! - [Function]
//! - [Label]
//! - [Location] and [Line]
//! - [Mapping]
//! - [Sample]
//! - [ValueType]
//!
//! There is no serializer for Profile. It would require borrowing a lot of
//! data, which becomes unwieldy. It also isn't very compatible with writing
//! a streaming serializer to lower peak memory usage.
//!
//! Indices into the string table are represented by [StringOffset], which uses
//! a 32-bit number. ID fields are still 64-bit, so the user can control their
//! values, potentially using a 64-bit address for its value.
//!
//! The types are generally `#[repr(C)]` so they can be used in FFI one day.
//!
//! [`profiles`]: https://github.com/google/pprof/blob/main/proto/profile.proto

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

/// Represents the wire type for the in-wire protobuf encoding. There are more
/// types than are represented here; these are just the supported ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}

/// A value is stored differently depending on the wire_type.
pub trait Value {
    const WIRE_TYPE: WireType;

    /// The number of bytes it takes to encode this value.
    fn proto_len(&self) -> u64;

    /// Encode the value to the in-wire protobuf format.
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()>;

    /// Create a Pair with the given field. The wire type will be added
    /// implicitly, and will be this type's [Self::WIRE_TYPE].
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
    /// Calculate the size of pair, without the zero-size optimization.
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

    /// Encodes the pair into protobuf, without the zero-size optimization.
    ///
    /// # Examples
    ///
    /// Given a message like:
    ///
    /// ```protobuf
    /// message ValueType {
    ///   int64 type = 1;
    ///   int64 unit = 2;
    /// }
    /// ```
    ///
    /// You can encode it like this:
    ///
    /// ```
    /// # use datadog_profiling_protobuf::{Value, Varint};
    /// # struct ValueType { r#type: i64, unit: i64 }
    /// # fn main() -> std::io::Result<()> {
    /// let mut w = Vec::new();
    /// let value_type = ValueType { r#type: 4, unit: 5 };
    /// Varint::from(value_type.r#type).field(1).encode(&mut w)?;
    /// Varint::from(value_type.unit).field(2).encode(&mut w)?;
    /// # Ok(()) }
    /// ```
    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        Tag::new(self.field, V::WIRE_TYPE).encode(writer)?;
        if V::WIRE_TYPE == WireType::LengthDelimited {
            let len = self.value.proto_len();
            Varint(len).encode(writer)?;
        }
        self.value.encode(writer)
    }

    /// Convert the pair into one that will apply the zero-size optimization.
    ///
    /// Note that the zero-size optimization should be applied to the field
    /// consistently in its [Value::proto_len] and [Value::encode] methods.
    /// If it's done to one, it should be done to the other.
    #[inline]
    pub fn zero_opt(self) -> WithZeroOptimization<V> {
        WithZeroOptimization { pair: self }
    }
}

pub struct WithZeroOptimization<V: Value> {
    pair: Pair<V>,
}

impl<V: Value> WithZeroOptimization<V> {
    /// Calculate the size of pair, using the zero-size optimization.
    #[inline]
    pub fn proto_len(&self) -> u64 {
        if self.pair.value.proto_len() != 0 {
            self.pair.proto_len()
        } else {
            0
        }
    }

    /// Encodes into protobuf, using the zero-size optimization. Protobuf
    /// doesn't require fields with values of zero to be present, so to save
    /// space, they can be omitted them altogether.
    ///
    /// # Examples
    ///
    /// Label is a great message to demonstrate how the optimization is useful
    /// because it has multiple optional values:
    ///
    /// ```protobuf
    /// message Label {
    ///   int64 key = 1;
    ///
    ///   // At most one of the following must be present
    ///   int64 str = 2;
    ///   int64 num = 3;
    ///
    ///   // Should only be present when num is present.
    ///   int64 num_unit = 4;
    /// }
    /// ```
    ///
    /// This can be taken advantage of by using `zero_opt`:
    ///
    /// ```
    /// # use datadog_profiling_protobuf::{Value, Varint};
    /// # struct Label { key: i64, str: i64, num: i64, num_unit: i64 }
    /// # fn main() -> std::io::Result<()> {
    /// let mut w = Vec::new();
    ///
    /// let label = Label {
    ///     key: 1,
    ///     str: 0,
    ///     num: 4194303,
    ///     num_unit: 0,
    /// };
    ///
    /// Varint::from(label.key).field(1).zero_opt().encode(&mut w)?;
    /// Varint::from(label.str).field(2).zero_opt().encode(&mut w)?;
    /// Varint::from(label.num).field(3).zero_opt().encode(&mut w)?;
    /// Varint::from(label.num_unit)
    ///     .field(4)
    ///     .zero_opt()
    ///     .encode(&mut w)?;
    /// # Ok(()) }
    /// ```
    #[inline]
    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        let len = self.pair.value.proto_len();
        if len == 0 {
            return Ok(());
        }

        Tag::new(self.pair.field, V::WIRE_TYPE).encode(writer)?;
        if V::WIRE_TYPE == WireType::LengthDelimited {
            Varint(len).encode(writer)?;
        }
        self.pair.value.encode(writer)
    }
}

/// The smallest possible protobuf field number.
const MIN_FIELD: u32 = 1;

/// The largest possible protobuf field number.
const MAX_FIELD: u32 = (1 << 29) - 1;

/// A tag is a combination of a wire_type, stored in the least significant
/// three bits, and the field number that is defined in the .proto file.
#[derive(Copy, Clone)]
pub struct Tag(u32);

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

/// Represents a packed varint. There are other kinds of things which can be
/// packed in protobuf, but profiles don't currently use them.
///
/// # Examples
///
/// Packed is generic over `Into<Varint>`, so packed values of i64 and u64 can
/// both be used.
///
/// ```
/// # use datadog_profiling_protobuf::Packed;
/// // u64
/// _ = Packed::new(&[42u64, 67u64]);
/// // i64
/// _ = Packed::new(&[42i64, 67i64]);
/// ```
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
