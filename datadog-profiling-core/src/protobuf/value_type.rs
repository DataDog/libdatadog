// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode, Buffer, ByteRange, LenEncodable, StringOffset};
use datadog_alloc::buffer::MayGrowOps;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ValueType {
    pub r#type: StringOffset, // 1
    pub unit: StringOffset,   // 2
}

impl ValueType {
    pub const fn encoded_len(&self) -> usize {
        encode::tagged_varint_len(1, self.r#type.offset as u64)
            + encode::tagged_varint_len(2, self.unit.offset as u64)
    }
}

impl LenEncodable for ValueType {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        let start = buffer.len_u31();
        encode::tagged_varint(buffer, 1, self.r#type.offset as u64);
        encode::tagged_varint(buffer, 2, self.unit.offset as u64);
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}
