// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Buffer, ByteRange, StringOffset};
use crate::protobuf::{encode, Identifiable, LenEncodable};
use datadog_alloc::buffer::MayGrowOps;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Mapping {
    pub id: u64,                // 1
    pub memory_start: u64,      // 2
    pub memory_limit: u64,      // 3
    pub file_offset: u64,       // 4
    pub filename: StringOffset, // 5
    pub build_id: StringOffset, // 6
}

impl Mapping {
    const MAX: Mapping = {
        // SAFETY: we're using this for calculating the size of the buffer
        // that we need, not encoding/using this otherwise.
        let max_string_id = unsafe { StringOffset::new_unchecked(u32::MAX) };
        Mapping {
            id: u64::MAX,
            memory_start: u64::MAX,
            memory_limit: u64::MAX,
            file_offset: u64::MAX,
            filename: max_string_id,
            build_id: max_string_id,
        }
    };

    /// The number of bytes needed to encode any possible Mapping.
    pub const MAX_ENCODED_LEN: usize = {
        let len = Self::MAX.encode_len();
        len + encode::varint_len(len as u64)
            + encode::key_len(encode::MAX_TAG, encode::WireType::LengthDelimited)
        // FYI, the last time I checked this size it was 64 bytes.
    };

    pub const fn encode_len(&self) -> usize {
        encode::tagged_varint_len_without_zero_size_opt(1, self.id)
            + encode::tagged_varint_len(2, self.memory_start)
            + encode::tagged_varint_len(3, self.memory_limit)
            + encode::tagged_varint_len(4, self.file_offset)
            + encode::tagged_varint_len(5, self.filename.offset as u64)
            + encode::tagged_varint_len(6, self.build_id.offset as u64)
    }
}

impl LenEncodable for Mapping {
    fn encoded_len(&self) -> usize {
        self.encode_len()
    }

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        encode::tagged_varint_without_zero_size_opt(buffer, 1, self.id);
        let start = buffer.len_u31();
        encode::tagged_varint(buffer, 2, self.memory_start);
        encode::tagged_varint(buffer, 3, self.memory_limit);
        encode::tagged_varint(buffer, 4, self.file_offset);
        encode::tagged_varint(buffer, 5, self.filename.offset as u64);
        encode::tagged_varint(buffer, 6, self.build_id.offset as u64);
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}

impl Identifiable for Mapping {
    fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::prost_impls;
    use std::io;

    #[test]
    fn roundtrip() -> io::Result<()> {
        let max_mapping = Mapping {
            id: u64::MAX,
            memory_start: u64::MAX,
            memory_limit: u64::MAX,
            file_offset: u64::MAX,
            filename: StringOffset { offset: u32::MAX },
            build_id: StringOffset { offset: u32::MAX },
        };

        let min_mapping = Mapping {
            id: 0,
            memory_start: 0,
            memory_limit: 0,
            file_offset: 0,
            filename: StringOffset::ZERO,
            build_id: StringOffset::ZERO,
        };

        let mut buffer = std::vec::Vec::new();
        let test = |buffer: &mut std::vec::Vec<u8>, mapping: Mapping| -> io::Result<()> {
            use prost::Message;
            let prost_mapping = prost_impls::Mapping {
                id: mapping.id,
                memory_start: mapping.memory_start,
                memory_limit: mapping.memory_limit,
                file_offset: mapping.file_offset,
                filename: mapping.filename.offset as i64,
                build_id: mapping.build_id.offset as i64,
                has_functions: false,
                has_filenames: false,
                has_line_numbers: false,
                has_inline_frames: false,
            };

            prost_mapping.encode(buffer)?;
            let roundtrip = prost_impls::Mapping::decode(&buffer[..])?;
            assert_eq!(prost_mapping, roundtrip);
            Ok(())
        };

        test(&mut buffer, max_mapping)?;
        buffer.clear();
        test(&mut buffer, min_mapping)?;
        Ok(())
    }
}
