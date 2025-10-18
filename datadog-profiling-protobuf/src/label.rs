// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Record, StringOffset, Value, WireType, OPT_ZERO};
use std::hash::Hash;
use std::io::{self, Write};

/// A label includes additional context for this sample. It can include things
/// like a thread id, allocation size, etc.
/// This repr omits `num_unit` to save 8 bytes (4 from padding).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "bolero", derive(bolero::generator::TypeGenerator))]
pub struct Label {
    /// An annotation for a sample, e.g. "allocation_size".
    pub key: Record<StringOffset, 1, OPT_ZERO>,
    /// At most, one of the str and num should be used.
    pub str: Record<StringOffset, 2, OPT_ZERO>,
    /// At most, one of the str and num should be used.
    pub num: Record<i64, 3, OPT_ZERO>,
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for Label {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.key.proto_len() + self.str.proto_len() + self.num.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.key.encode(writer)?;
        self.str.encode(writer)?;
        self.num.encode(writer)
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
            key: label.key.value.into(),
            str: label.str.value.into(),
            num: label.num.value,
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
            assert_eq!(i64::from(label.key.value), prost_label.key);
            assert_eq!(i64::from(label.str.value), prost_label.str);
            assert_eq!(label.num.value, prost_label.num);

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
