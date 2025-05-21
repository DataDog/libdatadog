// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode, LenEncodable, StringOffset};
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ValueType {
    pub r#type: StringOffset, // 1
    pub unit: StringOffset,   // 2
}

impl ValueType {
    const MAX: ValueType = {
        // Not using as an offset, just need the largest possible string id,
        // and this is even a bit more generous than is necessary.
        let max_string_id = unsafe { StringOffset::new_unchecked(u32::MAX) };
        ValueType {
            r#type: max_string_id,
            unit: max_string_id,
        }
    };

    /// The number of bytes needed to encode any possible ValueType.
    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encoded_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
    };
    pub const fn encoded_len(&self) -> usize {
        encode::tagged_varint_len(1, self.r#type.offset as u64)
            + encode::tagged_varint_len(2, self.unit.offset as u64)
    }
}

impl LenEncodable for ValueType {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut storage: [mem::MaybeUninit<u8>; Self::MAX_ENCODED_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; Self::MAX_ENCODED_LEN]>::uninit()) };
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe {
            encode::tagged_varint(&mut buf, 1, self.r#type.offset as u64);
            encode::tagged_varint(&mut buf, 2, self.unit.offset as u64);
        }
        writer.write_all(buf.as_slice())
    }
}
