// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode, LenEncodable, StringOffset};
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

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
    const MAX: Label = {
        // Not using as an offset, just need the largest possible string id,
        // and this is even a bit more generous than is necessary.
        let max_string_id = unsafe { StringOffset::new_unchecked(u32::MAX) };
        Label {
            key: max_string_id,
            str: StringOffset::ZERO,
            num: i64::MIN,
            num_unit: max_string_id,
        }
    };

    /// The number of bytes needed to encode any possible Label.
    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encoded_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
    };

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

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut storage: [mem::MaybeUninit<u8>; Self::MAX_ENCODED_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; Self::MAX_ENCODED_LEN]>::uninit()) };
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe {
            encode::tagged_varint(&mut buf, 1, self.key.offset as u64);
            encode::tagged_varint(&mut buf, 2, self.str.offset as u64);
            encode::tagged_varint(&mut buf, 3, self.num as u64);
            encode::tagged_varint(&mut buf, 4, self.num_unit.offset as u64);
        }
        writer.write_all(buf.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use std::io;

    #[test]
    fn roundtrip() -> io::Result<()> {
        let max_label = Label::MAX;

        // Not sure why you'd use this label but...
        let min_label = Label {
            key: StringOffset::ZERO,
            str: StringOffset::ZERO,
            num: 0,
            num_unit: StringOffset::ZERO,
        };

        let str_label = Label {
            key: StringOffset {
                offset: u16::MAX as u32,
            },
            str: StringOffset {
                offset: u8::MAX as u32,
            },
            num: 0,
            num_unit: StringOffset::ZERO,
        };

        let max_str_label = Label {
            key: StringOffset { offset: u32::MAX },
            str: StringOffset { offset: u32::MAX },
            num: 0,
            num_unit: StringOffset::ZERO,
        };

        let mut buffer = Vec::new();
        let test = |buffer: &mut Vec<u8>, label: Label| -> io::Result<()> {
            use prost::Message;
            let prost_label = prost_impls::Label {
                key: label.key.offset as i64,
                str: label.str.offset as i64,
                num: label.num,
                num_unit: label.num_unit.offset as i64,
            };

            prost_label.encode(buffer)?;
            let roundtrip = prost_impls::Label::decode(&buffer[..])?;
            assert_eq!(prost_label, roundtrip);
            Ok(())
        };

        test(&mut buffer, max_label)?;
        buffer.clear();
        test(&mut buffer, min_label)?;
        buffer.clear();
        test(&mut buffer, str_label)?;
        buffer.clear();
        test(&mut buffer, max_str_label)?;
        buffer.clear();
        Ok(())
    }
}
