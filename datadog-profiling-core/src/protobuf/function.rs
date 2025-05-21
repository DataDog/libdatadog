// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::StringOffset;
use crate::protobuf::{encode, Identifiable, LenEncodable};
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Function {
    pub id: u64,                   // 1
    pub name: StringOffset,        // 2
    pub system_name: StringOffset, // 3
    pub filename: StringOffset,    // 4
}

impl Function {
    const MAX: Function = {
        // Not using as an offset, just need the largest possible string id,
        // and this is even a bit generous then is necessary.
        let max_string_id = unsafe { StringOffset::new_unchecked(u32::MAX) };
        Function {
            id: u64::MAX,
            name: max_string_id,
            system_name: max_string_id,
            filename: max_string_id,
        }
    };

    /// The number of bytes needed to encode any possible Function.
    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encoded_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
        // FYI, the last time I checked this size it was 62 bytes.
    };

    pub const fn encoded_len(&self) -> usize {
        encode::tagged_varint_len_without_zero_size_opt(1, self.id)
            + encode::tagged_varint_len(2, self.name.offset as u64)
            + encode::tagged_varint_len(3, self.system_name.offset as u64)
            + encode::tagged_varint_len(4, self.filename.offset as u64)
    }
}

impl LenEncodable for Function {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // todo: is it better to write to writer, using a BufWriter if needed?
        let mut local_buf: [mem::MaybeUninit<u8>; Self::MAX_ENCODED_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; Self::MAX_ENCODED_LEN]>::uninit()) };
        let mut buffer = FixedCapacityBuffer::from(local_buf.as_mut_slice());
        unsafe {
            encode::tagged_varint_without_zero_size_opt(&mut buffer, 1, self.id);
            encode::tagged_varint(&mut buffer, 2, self.name.offset as u64);
            encode::tagged_varint(&mut buffer, 3, self.system_name.offset as u64);
            encode::tagged_varint(&mut buffer, 4, self.filename.offset as u64);
        }
        writer.write_all(buffer.as_slice())
    }
}

impl Identifiable for Function {
    fn id(&self) -> u64 {
        self.id
    }
}
