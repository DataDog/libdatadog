// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{StringOffset, Value, Varint, WireType};
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

impl Mapping {}

impl Value for Mapping {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        Varint(self.id).field(1).proto_len()
            + Varint(self.memory_start).field(2).proto_len_small()
            + Varint(self.memory_limit).field(3).proto_len_small()
            + Varint(self.file_offset).field(4).proto_len_small()
            + Varint::from(self.filename).field(5).proto_len_small()
            + Varint::from(self.build_id).field(6).proto_len_small()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.id).field(1).encode(writer)?;
        Varint(self.memory_start).field(2).encode_small(writer)?;
        Varint(self.memory_limit).field(3).encode_small(writer)?;
        Varint(self.file_offset).field(4).encode_small(writer)?;
        Varint::from(self.filename).field(5).encode_small(writer)?;
        Varint::from(self.build_id).field(6).encode_small(writer)
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
            ..Self::default()
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
            let mut buffer = Vec::with_capacity(mapping.proto_len() as usize);
            mapping.encode(&mut buffer).unwrap();
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
