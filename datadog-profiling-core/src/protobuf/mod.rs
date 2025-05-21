// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod function;
mod label;
mod location;
mod mapping;
mod sample;
mod string;
mod value_type;

pub use datadog_alloc::buffer::NoGrowOps;
pub use function::*;
pub use label::*;
pub use location::*;
pub use mapping::*;
pub use sample::*;
pub use string::*;
pub use value_type::*;

use crate::protobuf::encode::WireType;
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

mod sealed {
    use super::*;
    use crate::protobuf::value_type::ValueType;

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

pub trait LenEncodable: sealed::Sealed {
    fn encoded_len(&self) -> usize;

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

#[inline]
pub const fn tagged_len_delimited_len(tag: u32, len: u64) -> usize {
    encode::key_len(tag, WireType::LengthDelimited) + encode::varint_len(len)
}

pub fn encoded_len<L: LenEncodable>(tag: u32, l: &L) -> (usize, usize) {
    let len = l.encoded_len();
    let needed = tagged_len_delimited_len(tag, len as u64) + len;
    (len, needed)
}

pub fn encode_len_delimited<L, W>(writer: &mut W, tag: u32, l: &L, len: usize) -> io::Result<()>
where
    L: LenEncodable,
    W: Write,
{
    debug_assert_eq!(len, encoded_len(tag, l).0);
    const BUFFER_LEN: usize = tagged_len_delimited_len(encode::MAX_TAG, u64::MAX);
    let mut storage: [mem::MaybeUninit<u8>; BUFFER_LEN] =
        unsafe { mem::transmute(mem::MaybeUninit::<[u8; BUFFER_LEN]>::uninit()) };
    let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
    unsafe {
        encode::key(&mut buf, tag, WireType::LengthDelimited);
        encode::varint(&mut buf, len as u64);
    }
    writer.write_all(buf.as_slice())?;
    l.encode_raw(writer)
}

pub trait Identifiable: LenEncodable {
    fn id(&self) -> u64;
}

pub mod encode {
    use super::*;
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

    /// # Safety
    /// There must be enough bytes to encode this varint. Varints take between
    /// 1 and 10 bytes to encode.
    #[inline]
    pub unsafe fn varint<B: NoGrowOps<u8>>(buf: &mut B, mut value: u64) {
        debug_assert!(varint_len(value) <= buf.remaining_capacity());
        loop {
            let byte = if value < 0x80 {
                value as u8
            } else {
                ((value & 0x7F) | 0x80) as u8
            };
            // SAFETY: derives from this function's safety conditions.
            unsafe { buf.push_within_capacity(byte) };
            if value < 0x80 {
                break;
            }
            value >>= 7;
        }
    }

    /// # Safety
    /// The buffer should have enough bytes to store the varint. A 64-bit
    /// varint never takes more than 10 bytes.
    #[inline]
    pub unsafe fn tagged_varint<B: NoGrowOps<u8>>(buf: &mut B, tag: u32, value: u64) {
        if value != 0 {
            tagged_varint_without_zero_size_opt(buf, tag, value);
        }
    }

    /// # Safety
    /// The buffer should have enough bytes to store the tagged varint.
    #[inline]
    pub unsafe fn tagged_varint_without_zero_size_opt<B: NoGrowOps<u8>>(
        buf: &mut B,
        tag: u32,
        value: u64,
    ) {
        debug_assert!(
            tagged_varint_len_without_zero_size_opt(tag, value) <= buf.remaining_capacity()
        );
        key(buf, tag, WireType::Varint);
        varint(buf, value)
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

    /// # Safety
    /// There must be enough space to encode the key.
    #[cfg_attr(debug_assertions, track_caller)]
    #[inline]
    pub unsafe fn key<B: NoGrowOps<u8>>(buf: &mut B, tag: u32, wire_type: WireType) {
        debug_assert!((MIN_TAG..=MAX_TAG).contains(&tag));
        let key = (tag << 3) | wire_type as u32;
        unsafe { varint(buf, u64::from(key)) };
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
}
