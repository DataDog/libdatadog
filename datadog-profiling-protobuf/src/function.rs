// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{StringOffset, Value, Varint, WireType};
use std::io::{self, Write};

/// Represents a function in a profile. Omits the start line because it's not
/// useful to libdatadog right now, so we save the bytes/ops.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Function {
    /// Unique nonzero id for the function.
    pub id: u64, // 1
    /// Name of the function, in human-readable form if available.
    pub name: StringOffset, // 2
    /// Name of the function, as identified by the system.
    /// For instance, it can be a C++ mangled name.
    pub system_name: StringOffset, // 3
    /// Source file containing the function.
    pub filename: StringOffset, // 4
}

impl Value for Function {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        Varint(self.id).field(1).proto_len()
            + Varint::from(self.name).field(2).zero_opt().proto_len()
            + Varint::from(self.system_name)
                .field(3)
                .zero_opt()
                .proto_len()
            + Varint::from(self.filename).field(4).zero_opt().proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.id).field(1).encode(writer)?;
        Varint::from(self.name).field(2).zero_opt().encode(writer)?;
        Varint::from(self.system_name)
            .field(3)
            .zero_opt()
            .encode(writer)?;
        Varint::from(self.filename)
            .field(4)
            .zero_opt()
            .encode(writer)
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
