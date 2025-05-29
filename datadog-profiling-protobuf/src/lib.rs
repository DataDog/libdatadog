// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! This crate implements Protobuf encoders for [`profiles`] which write to a
//! [`Write`]. It has encoders for:
//!
//! - [Function]
//! - [Label]
//! - [Location] and [Line]
//! - [Mapping]
//! - [Sample]
//! - [ValueType]
//!
//! There is no encoder for a Profile message. It would require borrowing a
//! lot of data, which becomes unwieldy. It also isn't very compatible with
//! writing a streaming serializer to lower peak memory usage.
//!
//! Encoding often happens one byte at a time, so a buffered writer should
//! probably be used.
//!
//! Indices into the string table are represented by [StringOffset], which uses
//! a 32-bit number. ID fields are still 64-bit, so the user can control their
//! values, potentially using a 64-bit address for its value.
//!
//! The types are generally `#[repr(C)]` so they can be used in FFI one day.
//!
//! Here is a condensed reference for the parts of protobuf used by profiles:
//!
//! ```reference
//! message    := (tag value)*
//! tag        := (field << 3) bit-or wire_type;
//!                 encoded as uint32 varint
//! value      := varint      for wire_type == VARINT,
//!               len-prefix  for wire_type == LEN,
//! varint     := int64 | uint64
//! len-prefix := size (message | string | packed);
//!                 size encoded as int32 varint
//! string     := valid UTF-8 string;
//!                 max 2GB of bytes
//! packed     := varint*
//!                 consecutive values of the type specified in `.proto`
//! ```
//!
//! A [`Record`] represents a [`Tag`] and [`Value`] pair, where the
//! [`WireType`] comes from [`Value::WIRE_TYPE`].
//!
//! Protos must be smaller than 2 GiB when encoded. Many proto implementations
//! will refuse to encode or decode messages that exceed this limit.
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

use std::fmt::{Debug, Formatter};
use std::io::{self, Write};

/// A record is responsible for encoding the field number, wire type and
/// payload. The wire type tells the parser how big the payload after it is.
/// For more details, refer to the [Condensed Reference Card].
///
/// The `P` is the payload, the `F` is the field number, and `O` is whether to
/// apply the zero-sized optimization or not. Most of the time, it shouldn't
/// matter if the optimization is applied. However, if something is part of
/// a repeated field, then applying the optimization would change the number
/// of elements in the array.
///
/// [Condensed Reference Card]: https://protobuf.dev/programming-guides/encoding/#cheat-sheet
#[derive(Copy, Clone, Default, Eq, PartialEq)]
#[repr(transparent)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Record<P: Value, const F: u32, const O: bool> {
    pub value: P,
}

/// Represents the wire type for the in-wire protobuf encoding. There are more
/// types than are represented here; these are just the ones used in profiles.
/// See [Message Structure] for more documentation.
///
/// [Message Structure]: https://protobuf.dev/programming-guides/encoding/#structure
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}

/// A value (or payload) is stored differently depending on the wire_type. In
/// profiles, there two types of payloads: varints and len-prefixed types.
///
/// # Safety
///
/// The [`Default`] implementation _must_ provide all zero values.
pub unsafe trait Value: Default + Eq {
    /// The wire type this value uses.
    const WIRE_TYPE: WireType;

    /// The number of bytes it takes to encode this value. Do not include the
    /// number of bytes it takes to encode the length-prefix as a varint. For
    /// example, using this snippet of the reference:
    ///
    /// ```reference
    /// len-prefix := size (message | string | packed);
    ///                size encoded as int32 varint
    /// ```
    ///
    /// Calculate the number of bytes for `(message |  string | packed)` only.
    ///
    /// For a varint, returns between 1 and 10 bytes for the number of bytes
    /// used to encode the varint.
    ///
    /// Returns u64 rather than u31 to avoid a lot of overflow checking.
    fn proto_len(&self) -> u64;

    /// Encode the value to the in-wire protobuf format. For length-delimited
    /// types, do not include the length-prefix; see [`Value::proto_len`] for
    /// more details.
    ///
    /// Encoding often happens one byte at a time, so a buffered writer should
    /// probably be used.
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

/// Intended to be provided to a Field to mean that it _should_ optimize for a
/// value of zero. See also [`NO_OPT_ZERO`].
pub const OPT_ZERO: bool = true;

/// Intended to be provided to a Field to mean that it shouldn't optimize for a
/// value of zero. Should be used on fields that should not be zero, such as
/// Mapping.id.
pub const NO_OPT_ZERO: bool = false;

impl<P: Value, const F: u32, const O: bool> From<P> for Record<P, F, O> {
    fn from(value: P) -> Self {
        Record { value }
    }
}

unsafe impl<P: Value, const F: u32, const O: bool> Value for Record<P, F, O> {
    const WIRE_TYPE: WireType = P::WIRE_TYPE;

    fn proto_len(&self) -> u64 {
        if O && self.value == P::default() {
            return 0;
        }
        let proto_len = self.value.proto_len();
        let len = if P::WIRE_TYPE == WireType::LengthDelimited {
            proto_len.proto_len()
        } else {
            0
        };
        let tag = Tag::new(F, P::WIRE_TYPE).proto_len();
        tag + len + proto_len
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if O && self.value == P::default() {
            return Ok(());
        }
        Tag::new(F, P::WIRE_TYPE).encode(writer)?;
        if P::WIRE_TYPE == WireType::LengthDelimited {
            varint::encode(self.value.proto_len(), writer)?;
        }
        self.value.encode(writer)
    }
}

impl<P: Debug + Value, const F: u32, const O: bool> Debug for Record<P, F, O> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("value", &self.value)
            .field("number", &F)
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
        varint::proto_len(self.0 as u64)
    }

    #[inline]
    pub fn encode<W: Write>(self, writer: &mut W) -> io::Result<()> {
        varint::encode(self.0 as u64, writer)
    }
}

unsafe impl<T: Value> Value for &'_ [T] {
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
