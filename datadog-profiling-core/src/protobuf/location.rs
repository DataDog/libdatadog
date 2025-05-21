// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::encode_len_delimited;
use crate::protobuf::{encode, LenEncodable};
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Location {
    pub id: u64,         // 1
    pub mapping_id: u64, // 2
    pub address: u64,    // 3
    pub line: Line,      // 4
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Line {
    pub function_id: u64, // 1
    pub lineno: i64,      // 2
}

impl Line {
    const MAX: Line = Line {
        function_id: u64::MAX,
        lineno: i64::MIN, // yes, MIN > MAX when varint encoded
    };

    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encoded_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
    };

    pub const fn encoded_len(&self) -> usize {
        encode::tagged_varint_len(1, self.function_id)
            + encode::tagged_varint_len(2, self.lineno as u64)
    }
}
impl LenEncodable for Line {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut storage: [mem::MaybeUninit<u8>; Self::MAX_ENCODED_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; Self::MAX_ENCODED_LEN]>::uninit()) };
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe {
            encode::tagged_varint(&mut buf, 1, self.function_id);
            encode::tagged_varint(&mut buf, 2, self.lineno as u64);
        }
        writer.write_all(buf.as_slice())
    }
}

impl Location {
    const MAX: Location = Location {
        id: u64::MAX,
        mapping_id: u64::MAX,
        address: u64::MAX,
        line: Line::MAX,
    };

    /// The number of bytes needed to encode any possible Location.
    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encoded_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
        // FYI, the last time I checked this size it was 63 bytes.
    };

    pub const fn encoded_len(&self) -> usize {
        let base = encode::tagged_varint_len_without_zero_size_opt(1, self.id)
            + encode::tagged_varint_len(2, self.mapping_id)
            + encode::tagged_varint_len(3, self.address);

        let needed = {
            let len = self.line.encoded_len();
            len + encode::varint_len(len as u64)
                + encode::key_len(4, encode::WireType::LengthDelimited)
        };
        base + needed
    }
}

impl LenEncodable for Location {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut storage: [mem::MaybeUninit<u8>; Self::MAX_ENCODED_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; Self::MAX_ENCODED_LEN]>::uninit()) };
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe {
            encode::key(&mut buf, 1, encode::WireType::Varint);
            encode::varint(&mut buf, self.id);
            encode::tagged_varint(&mut buf, 2, self.mapping_id);
            encode::tagged_varint(&mut buf, 3, self.address);
            encode_len_delimited(&mut buf, 4, &self.line)?;
        }
        writer.write_all(buf.as_slice())
    }
}
