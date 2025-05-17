// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode, Buffer, ByteRange, LenEncodable, StringOffset};
use datadog_alloc::buffer::MayGrowOps;

// todo: if we don't use num_unit, then we can save 8 bytes--4 from num_unit
//       plus 4 from padding.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Label {
    pub key: StringOffset,      // 1
    pub str: StringOffset,      // 2
    pub num: i64,               // 3
    pub num_unit: StringOffset, // 4
}

impl Label {
    #[inline]
    pub const fn is_zero(&self) -> bool {
        0 == (self.num as u64 | (self.key.offset | self.str.offset | self.num_unit.offset) as u64)
    }

    #[inline]
    pub const fn encoded_len(&self) -> usize {
        encode::tagged_varint_len(1, self.key.offset as u64)
            + encode::tagged_varint_len(2, self.str.offset as u64)
            + encode::tagged_varint_len(3, self.num as u64)
            + encode::tagged_varint_len(4, self.num_unit.offset as u64)
    }
}

impl LenEncodable for Label {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        let start = buffer.len_u31();
        encode::tagged_varint(buffer, 1, self.key.offset as u64);
        encode::tagged_varint(buffer, 2, self.str.offset as u64);
        encode::tagged_varint(buffer, 3, self.num as u64);
        encode::tagged_varint(buffer, 4, self.num_unit.offset as u64);
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}
