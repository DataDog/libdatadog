// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{StringOffset, Value, Varint, WireType};
use std::io::{self, Write};

// todo: if we don't use num_unit, then we can save 8 bytes--4 from num_unit
//       plus 4 from padding.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Label {
    pub key: StringOffset,      // 1
    pub str: StringOffset,      // 2
    pub num: i64,               // 3
    pub num_unit: StringOffset, // 4
}

impl Value for Label {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn encoded_len(&self) -> u64 {
        Varint::from(self.key.to_u64()).field(1).encoded_len()
            + Varint(self.str.to_u64()).field(2).encoded_len_small()
            + Varint(self.num as u64).field(3).encoded_len_small()
            + Varint(self.num_unit.to_u64()).field(4).encoded_len_small()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint::from(self.key.to_u64()).field(1).encode(writer)?;
        Varint(self.str.into()).field(2).encode_small(writer)?;
        Varint(self.num as u64).field(3).encode_small(writer)?;
        Varint(self.num_unit.into()).field(4).encode_small(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<Label> for crate::prost_impls::Label {
    fn from(label: Label) -> Self {
        Self::from(&label)
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Label> for crate::prost_impls::Label {
    fn from(label: &Label) -> Self {
        // If the prost file is regenerated, this may pick up new members.
        #[allow(clippy::needless_update)]
        Self {
            key: label.key.into(),
            str: label.str.into(),
            num: label.num,
            num_unit: label.num_unit.into(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use prost::Message;

    #[test]
    fn roundtrip() {
        fn test(label: &Label) {
            let mut buffer = Vec::new();
            let prost_label = prost_impls::Label::from(label);
            assert_eq!(i64::from(label.key), prost_label.key);
            assert_eq!(i64::from(label.str), prost_label.str);
            assert_eq!(label.num, prost_label.num);
            assert_eq!(i64::from(label.num_unit), prost_label.num_unit);

            label.encode(&mut buffer).unwrap();
            let roundtrip = prost_impls::Label::decode(buffer.as_slice()).unwrap();
            assert_eq!(prost_label, roundtrip);

            // This doesn't need to strictly be true, but it currently it is
            // true and makes testing easier.
            let mut buffer2 = Vec::new();
            prost_label.encode(&mut buffer2).unwrap();
            let roundtrip2 = prost_impls::Label::decode(buffer2.as_slice()).unwrap();
            assert_eq!(roundtrip, roundtrip2);
        }

        bolero::check!().with_type::<Label>().for_each(test);
    }
}
