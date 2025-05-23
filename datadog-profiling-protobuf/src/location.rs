// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Tag, Value, Varint, WireType};
use std::io::{self, Write};

#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Location {
    pub id: u64,         // 1
    pub mapping_id: u64, // 2
    pub address: u64,    // 3
    pub line: Line,      // 4
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct Line {
    pub function_id: u64, // 1
    pub lineno: i64,      // 2
}

impl Value for Line {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn encoded_len(&self) -> u64 {
        Varint(self.function_id).field(1).encoded_len_small()
            + Varint(self.lineno as u64).field(2).encoded_len_small()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.function_id).field(1).encode_small(writer)?;
        Varint(self.lineno as u64).field(2).encode_small(writer)
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

    fn encoded_len(&self) -> u64 {
        let value = self.address;
        let value1 = self.mapping_id;
        let base = Varint(self.mapping_id).field(1).encoded_len()
            + Varint(value1).field(2).encoded_len_small()
            + Varint(value).field(3).encoded_len_small();

        let needed = {
            let self1 = &self.line;
            let value = self1.lineno as u64;
            let value1 = self1.function_id;
            let len = Varint(value1).field(1).encoded_len_small()
                + Varint(value).field(2).encoded_len_small();
            len + Varint(len).encoded_len() + Tag::new(4, WireType::LengthDelimited).encoded_len()
        };
        base + needed
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Varint(self.id).field(1).encode(writer)?;
        let value = self.mapping_id;
        Varint(value).field(2).encode_small(writer)?;
        let value = self.address;
        Varint(value).field(3).encode_small(writer)?;
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
