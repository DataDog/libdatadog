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

pub trait TagEncodable {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()>;
}

impl TagEncodable for u64 {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        tagged_varint(w, tag, *self)
    }
}

impl TagEncodable for i64 {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        (*self as u64).encode_with_tag(w, tag)
    }
}

pub trait LenEncodable: sealed::Sealed + TagEncodable {
    fn encoded_len(&self) -> usize;

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

pub trait Identifiable: LenEncodable {
    fn id(&self) -> u64;
}

mod sealed {
    use super::*;

    pub trait Sealed {}

    impl Sealed for &str {}
    impl Sealed for Mapping {}
    impl Sealed for Function {}
    impl Sealed for Label {}
    impl Sealed for Line {}
    impl Sealed for Location {}
    impl Sealed for Sample<'_> {}
    impl Sealed for ValueType {}
}

const MIN_TAG: u32 = 1;
pub const MAX_TAG: u32 = (1 << 29) - 1;
pub const MAX_VARINT_LEN: usize = 10;

/// Represents the wire type for in-wire protobuf. There are more types than
/// are represented here; these are just the supported ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    LengthDelimited = 2,
}

#[inline]
pub fn varint<W: Write>(writer: &mut W, mut value: u64) -> io::Result<()> {
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

#[must_use]
#[inline]
pub const fn varint_len(value: u64) -> usize {
    // https://github.com/google/protobuf/blob/3.3.x/src/google/protobuf/io/coded_stream.h#L1301-L1309
    ((((value | 1).leading_zeros() ^ 63) * 9 + 73) / 64) as usize
}

#[must_use]
#[inline]
pub const fn key_len(tag: u32, wire_type: WireType) -> usize {
    let key = (tag << 3) | wire_type as u32;
    varint_len(key as u64)
}

#[cfg_attr(debug_assertions, track_caller)]
#[inline]
pub fn key<W: Write>(writer: &mut W, tag: u32, wire_type: WireType) -> io::Result<()> {
    debug_assert!((MIN_TAG..=MAX_TAG).contains(&tag));
    let key = (tag << 3) | wire_type as u32;
    varint(writer, key as u64)
}

pub fn encoded_len<L: LenEncodable>(tag: u32, l: &L) -> (usize, usize) {
    let len = l.encoded_len();
    let needed = tagged_len_delimited_len(tag, len as u64) + len;
    (len, needed)
}

pub fn encode_len_delimited<L, W>(writer: &mut W, tag: u32, l: &L) -> io::Result<()>
where
    L: LenEncodable,
    W: Write,
{
    let len = encoded_len(tag, l).0 as u64;
    encode_len_delimited_prefix(writer, tag, len)?;
    l.encode_raw(writer)
}

// Non-generic over the type it's a prefix for, reduces code size.
fn encode_len_delimited_prefix<W: Write>(writer: &mut W, tag: u32, len: u64) -> io::Result<()> {
    key(writer, tag, WireType::LengthDelimited)?;
    varint(writer, len)
}

#[inline]
pub fn tagged_varint<W: Write>(writer: &mut W, tag: u32, value: u64) -> io::Result<()> {
    if value != 0 {
        tagged_varint_without_zero_size_opt(writer, tag, value)
    } else {
        Ok(())
    }
}

#[inline]
pub fn tagged_varint_without_zero_size_opt<W: Write>(
    writer: &mut W,
    tag: u32,
    value: u64,
) -> io::Result<()> {
    key(writer, tag, WireType::Varint)?;
    varint(writer, value)
}

#[must_use]
#[inline]
pub const fn tagged_varint_len(tag: u32, value: u64) -> usize {
    if value != 0 {
        key_len(tag, WireType::Varint) + varint_len(value)
    } else {
        0
    }
}

#[must_use]
#[inline]
pub const fn tagged_varint_len_without_zero_size_opt(tag: u32, value: u64) -> usize {
    key_len(tag, WireType::Varint) + varint_len(value)
}

#[inline]
pub const fn tagged_len_delimited_len(tag: u32, len: u64) -> usize {
    key_len(tag, WireType::LengthDelimited) + varint_len(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_varint_len() {
        assert_eq!(MAX_VARINT_LEN, 10);
        assert_eq!(MAX_VARINT_LEN, varint_len(u64::MAX));
    }
}
