// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Record, StringOffset, Value, WireType, NO_OPT_ZERO, OPT_ZERO};
use std::io::{self, Write};

/// Represents a function in a profile. Omits the start line because it's not
/// useful to Datadog right now, so we save the bytes/ops.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Function {
    /// Unique nonzero id for the function.
    pub id: Record<u64, 1, NO_OPT_ZERO>,
    /// Name of the function, in human-readable form if available.
    pub name: Record<StringOffset, 2, OPT_ZERO>,
    /// Name of the function, as identified by the system.
    /// For instance, it can be a C++ mangled name.
    pub system_name: Record<StringOffset, 3, OPT_ZERO>,
    /// Source file containing the function.
    pub filename: Record<StringOffset, 4, OPT_ZERO>,
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for Function {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.id.proto_len()
            + self.name.proto_len()
            + self.system_name.proto_len()
            + self.filename.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.id.encode(writer)?;
        self.name.encode(writer)?;
        self.system_name.encode(writer)?;
        self.filename.encode(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Function> for crate::prost_impls::Function {
    fn from(value: &Function) -> Self {
        // If the prost file is regenerated, this may pick up new members,
        // such as start_line.
        #[allow(clippy::needless_update)]
        Self {
            id: value.id.value,
            name: value.name.value.into(),
            system_name: value.system_name.value.into(),
            filename: value.filename.value.into(),
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
