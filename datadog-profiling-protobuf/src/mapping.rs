// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode_len_delimited, StringOffset, TagEncodable};
use crate::{Identifiable, LenEncodable};
use std::io::{self, Write};

#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Mapping {
    pub id: u64,                // 1
    pub memory_start: u64,      // 2
    pub memory_limit: u64,      // 3
    pub file_offset: u64,       // 4
    pub filename: StringOffset, // 5
    pub build_id: StringOffset, // 6
}

impl Mapping {
    pub const fn encode_len(&self) -> usize {
        crate::tagged_varint_len_without_zero_size_opt(1, self.id)
            + crate::tagged_varint_len(2, self.memory_start)
            + crate::tagged_varint_len(3, self.memory_limit)
            + crate::tagged_varint_len(4, self.file_offset)
            + crate::tagged_varint_len(5, self.filename.to_u64())
            + crate::tagged_varint_len(6, self.build_id.to_u64())
    }
}

impl TagEncodable for Mapping {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for Mapping {
    fn encoded_len(&self) -> usize {
        self.encode_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint_without_zero_size_opt(writer, 1, self.id)?;
        crate::tagged_varint(writer, 2, self.memory_start)?;
        crate::tagged_varint(writer, 3, self.memory_limit)?;
        crate::tagged_varint(writer, 4, self.file_offset)?;
        crate::tagged_varint(writer, 5, self.filename.into())?;
        crate::tagged_varint(writer, 6, self.build_id.into())
    }
}

impl Identifiable for Mapping {
    fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Mapping> for crate::prost_impls::Mapping {
    fn from(mapping: &Mapping) -> Self {
        Self {
            id: mapping.id,
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename.into(),
            build_id: mapping.build_id.into(),
            has_functions: false,
            has_filenames: false,
            has_line_numbers: false,
            has_inline_frames: false,
        }
    }
}

#[cfg(feature = "prost_impls")]
impl From<Mapping> for crate::prost_impls::Mapping {
    fn from(mapping: Mapping) -> Self {
        Self::from(&mapping)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use prost::Message;

    fn test(mapping: &Mapping) {
        let prost_mapping = prost_impls::Mapping::from(mapping);
        assert_eq!(mapping.id, prost_mapping.id);
        assert_eq!(mapping.memory_start, prost_mapping.memory_start);
        assert_eq!(mapping.memory_limit, prost_mapping.memory_limit);
        assert_eq!(mapping.file_offset, prost_mapping.file_offset);
        assert_eq!(i64::from(mapping.filename), prost_mapping.filename);
        assert_eq!(i64::from(mapping.build_id), prost_mapping.build_id);

        let roundtrip = {
            let mut buffer = Vec::with_capacity(mapping.encoded_len());
            mapping.encode_raw(&mut buffer).unwrap();
            prost_impls::Mapping::decode(buffer.as_slice()).unwrap()
        };
        assert_eq!(roundtrip, prost_mapping);

        let roundtrip2 = {
            let mut buffer = Vec::with_capacity(prost_mapping.encoded_len());
            prost_mapping.encode(&mut buffer).unwrap();
            prost_impls::Mapping::decode(buffer.as_slice()).unwrap()
        };
        assert_eq!(roundtrip, roundtrip2);
    }

    #[test]
    fn roundtrip() {
        bolero::check!().with_type::<Mapping>().for_each(test);
    }
}
