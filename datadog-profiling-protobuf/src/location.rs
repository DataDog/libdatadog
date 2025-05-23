// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    encode_len_delimited, encode_len_delimited_prefix, tagged_len_delimited_len, LenEncodable,
    TagEncodable, WireType,
};
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

impl Line {
    pub const fn encoded_len(&self) -> usize {
        crate::tagged_varint_len(1, self.function_id)
            + crate::tagged_varint_len(2, self.lineno as u64)
    }
}

impl TagEncodable for Line {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for Line {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint(writer, 1, self.function_id)?;
        crate::tagged_varint(writer, 2, self.lineno as u64)
    }
}

#[cfg(feature = "prost_impls")]
impl From<Line> for crate::prost_impls::Line {
    fn from(line: Line) -> Self {
        Self {
            function_id: line.function_id,
            line: line.lineno,
        }
    }
}

impl Location {
    pub const fn encoded_len(&self) -> usize {
        let base = crate::tagged_varint_len_without_zero_size_opt(1, self.id)
            + crate::tagged_varint_len(2, self.mapping_id)
            + crate::tagged_varint_len(3, self.address);

        let needed = {
            let len = self.line.encoded_len();
            len + crate::varint_len(len as u64) + crate::key_len(4, WireType::LengthDelimited)
        };
        base + needed
    }
}

impl Location {
    fn encode_all_but_line<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint_without_zero_size_opt(writer, 1, self.id)?;
        crate::tagged_varint(writer, 2, self.mapping_id)?;
        crate::tagged_varint(writer, 3, self.address)
    }
}

impl TagEncodable for Location {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        // The whole point here is that we don't calculate the line_len twice.
        let (location_len, line_len) = {
            let base = crate::tagged_varint_len_without_zero_size_opt(1, self.id)
                + crate::tagged_varint_len(2, self.mapping_id)
                + crate::tagged_varint_len(3, self.address);

            let line_len = self.line.encoded_len();
            let total_line_len = tagged_len_delimited_len(4, line_len as u64) + line_len;
            let total_len = base + total_line_len;
            (total_len, line_len)
        };

        encode_len_delimited_prefix(w, tag, location_len as u64)?;
        self.encode_all_but_line(w)?;
        encode_len_delimited_prefix(w, 4, line_len as u64)?;
        self.line.encode_raw(w)
    }
}

impl LenEncodable for Location {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.encode_all_but_line(writer)?;
        encode_len_delimited(writer, 4, &self.line)
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

            location.encode_raw(&mut buffer).unwrap();
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
