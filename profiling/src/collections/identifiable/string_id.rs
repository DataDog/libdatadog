// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct StringId(u32);

impl StringId {
    pub const ZERO: StringId = Self::zero();

    #[inline]
    pub const fn zero() -> Self {
        Self(0)
    }

    #[inline]
    pub const fn is_zero(&self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}

impl Id for StringId {
    type RawId = i64;

    fn from_offset(inner: usize) -> Self {
        Self(inner.try_into().expect("StringId to fit into a u32"))
    }

    fn to_raw_id(self) -> Self::RawId {
        Self::RawId::from(self.0)
    }
}
