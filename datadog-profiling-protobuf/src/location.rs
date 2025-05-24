// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::varint::Varint;
use crate::{Value, WireType};
use std::io::{self, Write};

/// Describes function and line table debug information. This only supports a
/// single Line, whereas protobuf supports zero or more. The `is_folding`
/// field is not omitted for size/CPU reasons.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Location {
    /// Unique nonzero id for the location. A profile could use instruction
    /// addresses or any integer sequence as ids.
    pub id: u64, // 1
    /// The id of the corresponding profile.Mapping for this location.
    /// It can be unset if the mapping is unknown or not applicable for
    /// this profile type.
    pub mapping_id: u64, // 2
    /// The instruction address for this location, if available. It should be
    /// within `Mapping.memory_start..Mapping.memory_limit` for the
    /// corresponding mapping. A non-leaf address may be in the middle of a
    /// call instruction. It is up to display tools to find the beginning of
    /// the instruction if necessary.
    pub address: u64, // 3
    pub line: Line, // 4
}

/// Represents function and line number information. Omits column.  
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Line {
    /// The id of the corresponding profile.Function for this line.
    pub function_id: u64, // 1
    /// Line number in source code.
    pub lineno: i64, // 2
}

impl Value for Line {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        Varint(self.function_id).field(1).zero_opt().proto_len()
            + Varint::from(self.lineno).field(2).zero_opt().proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.function_id)
            .field(1)
            .zero_opt()
            .encode(writer)?;
        Varint::from(self.lineno).field(2).zero_opt().encode(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<Line> for crate::prost_impls::Line {
    fn from(line: Line) -> Self {
        // If the prost file is regenerated, this may pick up new members,
        // such as column.
        #[allow(clippy::needless_update)]
        Self {
            function_id: line.function_id,
            line: line.lineno,
            ..Self::default()
        }
    }
}

impl Value for Location {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        let base = Varint(self.id).field(1).proto_len()
            + Varint(self.mapping_id).field(2).zero_opt().proto_len()
            + Varint(self.address).field(3).zero_opt().proto_len();

        let line_len = self.line.field(4).proto_len();
        base + line_len
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.id).field(1).encode(writer)?;
        Varint(self.mapping_id).field(2).zero_opt().encode(writer)?;
        Varint(self.address).field(3).zero_opt().encode(writer)?;
        self.line.field(4).encode(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Location> for crate::prost_impls::Location {
    fn from(location: &Location) -> Self {
        Self {
            id: location.id,
            mapping_id: location.mapping_id,
            address: location.address,
            lines: vec![crate::prost_impls::Line::from(location.line)],
            is_folded: false,
        }
    }
}

#[cfg(feature = "prost_impls")]
impl From<Location> for crate::prost_impls::Location {
    fn from(location: Location) -> Self {
        Self::from(&location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use prost::Message;

    #[test]
    fn roundtrip() {
        fn test(location: &Location) {
            let mut buffer = Vec::new();
            let prost_location = prost_impls::Location::from(location);

            location.encode(&mut buffer).unwrap();
            let roundtrip = prost_impls::Location::decode(buffer.as_slice()).unwrap();
            assert_eq!(prost_location, roundtrip);

            // This doesn't need to strictly be true, but it currently it is
            // true and makes testing easier.
            let mut buffer2 = Vec::new();
            prost_location.encode(&mut buffer2).unwrap();
            let roundtrip2 = prost_impls::Location::decode(buffer2.as_slice()).unwrap();
            assert_eq!(roundtrip, roundtrip2);
        }

        bolero::check!().with_type::<Location>().for_each(test);
    }
}
