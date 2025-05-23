// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode_len_delimited, StringOffset, TagEncodable};
use crate::LenEncodable;
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

impl Label {
    #[inline]
    pub const fn encoded_len(&self) -> usize {
        crate::tagged_varint_len(1, self.key.to_u64())
            + crate::tagged_varint_len(2, self.str.to_u64())
            + crate::tagged_varint_len(3, self.num as u64)
            + crate::tagged_varint_len(4, self.num_unit.to_u64())
    }
}

impl TagEncodable for Label {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for Label {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint(writer, 1, self.key.into())?;
        crate::tagged_varint(writer, 2, self.str.into())?;
        crate::tagged_varint(writer, 3, self.num as u64)?;
        crate::tagged_varint(writer, 4, self.num_unit.into())
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
        Self {
            key: label.key.into(),
            str: label.str.into(),
            num: label.num,
            num_unit: label.num_unit.into(),
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

            label.encode_raw(&mut buffer).unwrap();
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
