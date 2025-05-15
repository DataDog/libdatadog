// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode_len_delimited, Buffer, ByteRange};
use crate::protobuf::{encode, LenEncodable};
use datadog_alloc::buffer::MayGrowOps;

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

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        let start = buffer.len_u31();
        encode::tagged_varint(buffer, 1, self.function_id);
        encode::tagged_varint(buffer, 2, self.lineno as u64);
        let end = buffer.len_u31();
        ByteRange { start, end }
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

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        encode::key(buffer, 1, encode::WireType::Varint);
        encode::varint(buffer, self.id);

        let start = buffer.len_u31();
        encode::tagged_varint(buffer, 2, self.mapping_id);
        encode::tagged_varint(buffer, 3, self.address);

        _ = encode_len_delimited(buffer, 4, &self.line, self.line.encoded_len());
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}
