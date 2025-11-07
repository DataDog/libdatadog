// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Record, Value, WireType, NO_OPT_ZERO, OPT_ZERO};
use std::io::{self, Write};

/// Describes function and line table debug information. This only supports a
/// single Line, whereas protobuf supports zero or more. The `is_folding`
/// field is not omitted for size/CPU reasons.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bolero", derive(bolero::generator::TypeGenerator))]
pub struct Location {
    /// Unique nonzero id for the location. A profile could use instruction
    /// addresses or any integer sequence as ids.
    pub id: Record<u64, 1, NO_OPT_ZERO>,
    /// The id of the corresponding profile.Mapping for this location.
    /// It can be unset if the mapping is unknown or not applicable for
    /// this profile type.
    pub mapping_id: Record<u64, 2, OPT_ZERO>,
    /// The instruction address for this location, if available. It should be
    /// within `Mapping.memory_start..Mapping.memory_limit` for the
    /// corresponding mapping. A non-leaf address may be in the middle of a
    /// call instruction. It is up to display tools to find the beginning of
    /// the instruction if necessary.
    pub address: Record<u64, 3, OPT_ZERO>,
    pub line: Record<Line, 4, OPT_ZERO>,
}

/// Represents function and line number information. Omits column.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bolero", derive(bolero::generator::TypeGenerator))]
pub struct Line {
    /// The id of the corresponding profile.Function for this line.
    pub function_id: Record<u64, 1, OPT_ZERO>,
    /// Line number in source code.
    pub lineno: Record<i64, 2, OPT_ZERO>,
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for Line {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.function_id.proto_len() + self.lineno.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.function_id.encode(writer)?;
        self.lineno.encode(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<Line> for crate::prost_impls::Line {
    fn from(line: Line) -> Self {
        // If the prost file is regenerated, this may pick up new members,
        // such as column.
        #[allow(clippy::needless_update)]
        Self {
            function_id: line.function_id.value,
            line: line.lineno.value,
            ..Self::default()
        }
    }
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for Location {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.id.proto_len()
            + self.mapping_id.proto_len()
            + self.address.proto_len()
            + self.line.proto_len()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.id.encode(writer)?;
        self.mapping_id.encode(writer)?;
        self.address.encode(writer)?;
        self.line.encode(writer)
    }
}

#[cfg(feature = "prost_impls")]
impl From<&Location> for crate::prost_impls::Location {
    fn from(location: &Location) -> Self {
        Self {
            id: location.id.value,
            mapping_id: location.mapping_id.value,
            address: location.address.value,
            lines: if location.line == Default::default() {
                Vec::new()
            } else {
                vec![crate::prost_impls::Line::from(location.line.value)]
            },
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

    #[track_caller]
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

    #[test]
    fn basic() {
        let location = Location {
            id: Record::default(),
            mapping_id: Record::default(),
            address: Record::default(),
            line: Record::from(Line {
                function_id: Record::from(1),
                lineno: Record::default(),
            }),
        };
        test(&location);
    }

    #[test]
    fn roundtrip() {
        bolero::check!().with_type::<Location>().for_each(test);
    }
}
