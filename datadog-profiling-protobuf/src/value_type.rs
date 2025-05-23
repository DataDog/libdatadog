// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{encode_len_delimited, LenEncodable, StringOffset, TagEncodable};
use std::io::{self, Write};

#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct ValueType {
    pub r#type: StringOffset, // 1
    pub unit: StringOffset,   // 2
}

impl ValueType {
    pub const fn encoded_len(&self) -> usize {
        crate::tagged_varint_len(1, self.r#type.to_u64())
            + crate::tagged_varint_len(2, self.unit.to_u64())
    }
}

impl TagEncodable for ValueType {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for ValueType {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint(writer, 1, self.r#type.into())?;
        crate::tagged_varint(writer, 2, self.unit.into())
    }
}

#[cfg(feature = "prost_impls")]
impl From<ValueType> for crate::prost_impls::ValueType {
    fn from(value: ValueType) -> Self {
        Self::from(&value)
    }
}

#[cfg(feature = "prost_impls")]
impl From<&ValueType> for crate::prost_impls::ValueType {
    fn from(value: &ValueType) -> Self {
        Self {
            r#type: value.r#type.into(),
            unit: value.unit.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use prost::Message;

    fn test(value_type: &ValueType) {
        let prost_value_type = prost_impls::ValueType::from(value_type);
        assert_eq!(i64::from(value_type.r#type), prost_value_type.r#type);
        assert_eq!(i64::from(value_type.unit), prost_value_type.unit);

        let len = value_type.encoded_len();
        let mut buffer = Vec::with_capacity(len);
        value_type.encode_raw(&mut buffer).unwrap();
        let roundtrip = prost_impls::ValueType::decode(buffer.as_slice()).unwrap();
        assert_eq!(prost_value_type, roundtrip);

        let mut buffer2 = Vec::with_capacity(prost_value_type.encoded_len());
        prost_value_type.encode(&mut buffer2).unwrap();
        let roundtrip2 = prost_impls::ValueType::decode(buffer2.as_slice()).unwrap();
        assert_eq!(roundtrip, roundtrip2);
    }

    #[test]
    fn roundtrip() {
        bolero::check!().with_type::<ValueType>().for_each(test);
    }
}
