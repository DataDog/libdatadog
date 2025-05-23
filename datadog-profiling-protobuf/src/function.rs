// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{StringOffset, Value, Varint, WireType};
use crate::Identifiable;
use std::io::{self, Write};

#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Function {
    pub id: u64,                   // 1
    pub name: StringOffset,        // 2
    pub system_name: StringOffset, // 3
    pub filename: StringOffset,    // 4
}

impl Value for Function {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn encoded_len(&self) -> u64 {
        Varint(self.id).field(1).encoded_len()
            + Varint(self.name.to_u64()).field(2).encoded_len_small()
            + Varint(self.system_name.to_u64())
                .field(3)
                .encoded_len_small()
            + Varint(self.filename.to_u64()).field(4).encoded_len_small()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.id).field(1).encode(writer)?;
        Varint(self.name.into()).field(2).encode_small(writer)?;
        Varint(self.system_name.into())
            .field(3)
            .encode_small(writer)?;
        Varint(self.filename.into()).field(4).encode_small(writer)
    }
}

impl Identifiable for Function {
    fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Function> for crate::prost_impls::Function {
    fn from(value: &Function) -> Self {
        // If the prost file is regenerated, this may pick up new members,
        // such as start_line.
        #[allow(clippy::needless_update)]
        Self {
            id: value.id,
            name: value.name.into(),
            system_name: value.system_name.into(),
            filename: value.filename.into(),
            ..Self::default()
        }
    }
}

#[cfg(feature = "prost_impls")]
impl From<Function> for crate::prost_impls::Function {
    fn from(value: Function) -> Self {
        Self::from(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use prost::Message;

    #[test]
    fn roundtrip() {
        fn test(function: &Function) {
            let mut buffer = Vec::new();
            let prost_function = prost_impls::Function::from(function);

            function.encode(&mut buffer).unwrap();
            let roundtrip = prost_impls::Function::decode(buffer.as_slice()).unwrap();
            assert_eq!(prost_function, roundtrip);

            // This doesn't need to strictly be true, but it currently it is
            // true and makes testing easier.
            let mut buffer2 = Vec::new();
            prost_function.encode(&mut buffer2).unwrap();
            let roundtrip2 = prost_impls::Function::decode(buffer2.as_slice()).unwrap();
            assert_eq!(roundtrip, roundtrip2);
        }

        bolero::check!().with_type::<Function>().for_each(test);
    }
}
