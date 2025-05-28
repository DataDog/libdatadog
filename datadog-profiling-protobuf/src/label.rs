// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Field, StringOffset, Value, WireType, OPT_ZERO};
use std::io::{self, Write};

/// Label includes additional context for this sample. It can include things
/// like a thread id, allocation size, etc.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Label {
    /// An annotation for a sample, e.g. "allocation_size".
    pub key: Field<StringOffset, 1, OPT_ZERO>,
    /// At most, one of the str and num should be used.
    pub str: Field<StringOffset, 2, OPT_ZERO>,
    /// At most, one of the str and num should be used.
    pub num: Field<i64, 3, OPT_ZERO>,

    // todo: if we don't use num_unit, then we can save 8 bytes--4 from
    //       num_unit plus 4 from padding.
    /// Should only be present when num is present.
    /// Specifies the units of num.
    /// Use arbitrary string (for example, "requests") as a custom count unit.
    /// If no unit is specified, consumer may apply heuristic to deduce it.
    pub num_unit: Field<StringOffset, 4, OPT_ZERO>,
}

impl Value for Label {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.key.proto_len()
            + self.str.proto_len()
            + self.num.proto_len()
            + self.num_unit.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.key.encode(writer)?;
        self.str.encode(writer)?;
        self.num.encode(writer)?;
        self.num_unit.encode(writer)
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
            key: label.key.value.into(),
            str: label.str.value.into(),
            num: label.num.value,
            num_unit: label.num_unit.value.into(),
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
            assert_eq!(i64::from(label.num_unit.value), prost_label.num_unit);

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
