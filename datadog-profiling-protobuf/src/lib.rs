// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! This crate implements Protobuf serializers for [`profiles`], including
//! serializers for:
//!
//! - [Function]
//! - [Label]
//! - [Location] and [Line]
//! - [Mapping]
//! - [Sample]
//! - [ValueType]
//!
//! Serialization often happens one byte at a time, so a buffered writer
//! should probably be used.
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
use std::fmt::{Debug, Formatter};
pub use string::*;
pub use value_type::*;

use std::io::{self, Write};

/// Create a field of a given type, field number, and whether to perform the
/// zero-size optimization or not.
#[derive(Copy, Clone, Default, Eq, PartialEq)]
#[repr(transparent)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Field<T: Value, const N: u32, const O: bool> {
    pub value: T,
}

/// Represents the wire type for the in-wire protobuf encoding. There are more
/// types than are represented here; these are just the supported ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}

/// A value is stored differently depending on the wire_type.
pub trait Value: Default + Eq {
    /// The wire type this value uses.
    const WIRE_TYPE: WireType;

    /// The number of bytes it takes to encode this value.
    fn proto_len(&self) -> u64;

    /// Encode the value to the in-wire protobuf format.
    ///
    /// Serialization often happens one byte at a time, so a buffered writer
    /// should probably be used.
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

/// You can use varint to store any of the listed data types:
/// int32 | int64 | uint32 | uint64 | bool | enum | sint32 | sint64
///
/// # Safety
///
/// The [`Value::WIRE_TYPE`] must be [`WireType::Varint`]!
pub unsafe trait Varint: Value + Sized {}

/// You can use LengthDelimited to store any of the listed data types:
/// string, bytes, embedded messages, packed repeated fields
///
/// # Safety
///
/// The [`Value::WIRE_TYPE`] must be [`WireType::LengthDelimited`]!
pub unsafe trait LengthDelimited: Value + Sized {}

/// Intended to be provided to a Field to mean that it _should_ optimize for a
/// value of zero. See also [`NO_OPT_ZERO`].
pub const OPT_ZERO: bool = true;

/// Intended to be provided to a Field to mean that it shouldn't optimize for a
/// value of zero. Should be used on fields that should not be zero, such as
/// Mapping.id.
pub const NO_OPT_ZERO: bool = false;

impl<T: Value, const N: u32, const O: bool> From<T> for Field<T, N, O> {
    fn from(value: T) -> Self {
        Field { value }
    }
}

impl<T: Value, const N: u32, const O: bool> Field<T, N, O> {
    pub fn proto_len(&self) -> u64 {
        if O && self.value == T::default() {
            return 0;
        }
        let proto_len = self.value.proto_len();
        let len = if T::WIRE_TYPE == WireType::LengthDelimited {
            proto_len.proto_len()
        } else {
            0
        };
        let tag = Tag::new(N, T::WIRE_TYPE).proto_len();
        tag + len + proto_len
    }

    pub fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if O && self.value == T::default() {
            return Ok(());
        }
        Tag::new(N, T::WIRE_TYPE).encode(writer)?;
        if T::WIRE_TYPE == WireType::LengthDelimited {
            self.value.proto_len().encode(writer)?;
        }
        self.value.encode(writer)
    }
}

impl<T: Debug + Value, const N: u32, const O: bool> Debug for Field<T, N, O> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("value", &self.value)
            .field("number", &N)
            .field("optimize_for_zero", &O)
            .finish()
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
        (self.0 as u64).proto_len()
    }

    #[inline]
    pub fn encode<W: Write>(self, writer: &mut W) -> io::Result<()> {
        (self.0 as u64).encode(writer)
    }
}
