// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{varint, Value, WireType};
use std::fmt;
use std::io::{self, Write};

unsafe impl Value for &str {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.len() as u64
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(self.as_bytes())
    }
}

// todo: for OTEL, needs to be i32::MAX rather than u32::MAX.
/// Represents an offset into the Profile's string table. Note that it cannot
/// exceed u32 because an entire protobuf message must not be larger than or
/// equal to 2 GiB. By the time you encode the tag and length prefix for each
/// string, there's no way to get this many unique-ish strings without first
/// exceeding the protobuf 2 GiB limit.
///
/// A value of 0 means "no string" or "empty string" (they are synonymous).
/// cbindgen:field-names=\[offset\]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "bolero", derive(bolero::generator::TypeGenerator))]
pub struct StringOffset(u32);

impl fmt::Display for StringOffset {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for StringOffset {
    const WIRE_TYPE: WireType = WireType::Varint;

    fn proto_len(&self) -> u64 {
        varint::proto_len(u64::from(self))
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        varint::encode(u64::from(self), writer)
    }
}

impl TryFrom<usize> for StringOffset {
    type Error = <u32 as TryFrom<usize>>::Error;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Ok(StringOffset(u32::try_from(value)?))
    }
}

impl TryFrom<&usize> for StringOffset {
    type Error = <u32 as TryFrom<usize>>::Error;

    fn try_from(value: &usize) -> Result<Self, Self::Error> {
        StringOffset::try_from(*value)
    }
}

impl From<StringOffset> for usize {
    fn from(s: StringOffset) -> Self {
        s.0 as usize
    }
}

impl From<&StringOffset> for usize {
    fn from(s: &StringOffset) -> Self {
        s.0 as usize
    }
}

impl From<u8> for StringOffset {
    fn from(value: u8) -> Self {
        StringOffset(value as u32)
    }
}

impl From<u16> for StringOffset {
    fn from(value: u16) -> Self {
        StringOffset(value as u32)
    }
}

impl From<u32> for StringOffset {
    fn from(value: u32) -> Self {
        StringOffset(value)
    }
}

impl From<&u32> for StringOffset {
    fn from(value: &u32) -> Self {
        StringOffset(*value)
    }
}

impl From<StringOffset> for u32 {
    fn from(s: StringOffset) -> Self {
        s.0
    }
}

impl From<&StringOffset> for u32 {
    fn from(s: &StringOffset) -> Self {
        s.0
    }
}

impl From<StringOffset> for u64 {
    fn from(s: StringOffset) -> Self {
        s.0 as u64
    }
}

impl From<&StringOffset> for u64 {
    fn from(s: &StringOffset) -> Self {
        s.0 as u64
    }
}

impl TryFrom<u64> for StringOffset {
    type Error = <u32 as TryFrom<u64>>::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(StringOffset(u32::try_from(value)?))
    }
}

impl TryFrom<&u64> for StringOffset {
    type Error = <u32 as TryFrom<u64>>::Error;

    fn try_from(value: &u64) -> Result<Self, Self::Error> {
        StringOffset::try_from(*value)
    }
}

impl From<StringOffset> for i64 {
    fn from(s: StringOffset) -> Self {
        s.0 as i64
    }
}

impl From<&StringOffset> for i64 {
    fn from(s: &StringOffset) -> Self {
        s.0 as i64
    }
}

impl TryFrom<i64> for StringOffset {
    type Error = <u32 as TryFrom<i64>>::Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Ok(StringOffset(u32::try_from(value)?))
    }
}

impl TryFrom<&i64> for StringOffset {
    type Error = <u32 as TryFrom<i64>>::Error;

    fn try_from(value: &i64) -> Result<Self, Self::Error> {
        StringOffset::try_from(*value)
    }
}

impl StringOffset {
    pub const ZERO: Self = Self(0);

    #[inline]
    pub const fn new(offset: u32) -> Self {
        Self(offset)
    }

    #[inline]
    pub const fn is_zero(&self) -> bool {
        self.0 == 0
    }
}
