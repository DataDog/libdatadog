// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode_len_delimited, StringOffset, TagEncodable};
use crate::{Identifiable, LenEncodable};
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

impl Function {
    pub const fn encoded_len(&self) -> usize {
        crate::tagged_varint_len_without_zero_size_opt(1, self.id)
            + crate::tagged_varint_len(2, self.name.to_u64())
            + crate::tagged_varint_len(3, self.system_name.to_u64())
            + crate::tagged_varint_len(4, self.filename.to_u64())
    }
}

impl TagEncodable for Function {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for Function {
    fn encoded_len(&self) -> usize {
        self.encoded_len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        crate::tagged_varint_without_zero_size_opt(writer, 1, self.id)?;
        crate::tagged_varint(writer, 2, self.name.into())?;
        crate::tagged_varint(writer, 3, self.system_name.into())?;
        crate::tagged_varint(writer, 4, self.filename.into())
    }
}

impl Identifiable for Function {
    fn id(&self) -> u64 {
        self.id
    }
}

impl From<&Function> for crate::prost_impls::Function {
    fn from(value: &Function) -> Self {
        Self {
            id: value.id,
            name: value.name.into(),
            system_name: value.system_name.into(),
            filename: value.filename.into(),
        }
    }
}

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

            function.encode_raw(&mut buffer).unwrap();
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
