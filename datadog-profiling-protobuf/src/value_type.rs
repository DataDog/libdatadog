// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Field, StringOffset, Value, WireType, OPT_ZERO};
use std::io::{self, Write};

/// ValueType describes the semantics and measurement units of a value.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct ValueType {
    pub r#type: Field<StringOffset, 1, OPT_ZERO>,
    pub unit: Field<StringOffset, 2, OPT_ZERO>,
}

impl Value for ValueType {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.r#type.proto_len() + self.unit.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.r#type.encode(writer)?;
        self.unit.encode(writer)
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
        // If the prost file is regenerated, this may pick up new members.
        #[allow(clippy::needless_update)]
        Self {
            r#type: value.r#type.value.into(),
            unit: value.unit.value.into(),
            ..Self::default()
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
        assert_eq!(i64::from(value_type.r#type.value), prost_value_type.r#type);
        assert_eq!(i64::from(value_type.unit.value), prost_value_type.unit);

        let mut buffer = Vec::with_capacity(value_type.proto_len() as usize);
        prost_value_type.encode(&mut buffer).unwrap();
        value_type.encode(&mut buffer).unwrap();
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
