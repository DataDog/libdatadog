// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::LenEncodable;
use std::io::{self, Write};

impl LenEncodable for &str {
    fn encoded_len(&self) -> usize {
        self.len()
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(self.as_bytes())
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StringOffset {
    pub(crate) offset: u32,
}

impl StringOffset {
    pub const ZERO: Self = Self { offset: 0 };

    /// # Safety
    /// The offset should exist in the string table. If it doesn't, then it
    /// shouldn't be looked up.
    pub const unsafe fn new_unchecked(offset: u32) -> Self {
        Self { offset }
    }
}
