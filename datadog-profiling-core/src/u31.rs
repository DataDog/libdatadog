// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Copy, Clone, Debug)]
#[allow(non_camel_case_types)]
pub struct u31(pub(crate) u32);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TryFromIntError(pub(crate) ());

impl u31 {
    pub const MAX: u31 = u31(u32::MAX >> 1);

    #[inline(always)]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// # Safety
    /// The value must be less than or equal to `i32::MAX`.
    #[inline]
    pub const unsafe fn new_unchecked(n: u32) -> u31 {
        u31(n)
    }
}

impl TryFrom<u32> for u31 {
    type Error = TryFromIntError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        const MASK: u32 = !u31::MAX.0;

        if value & MASK == 0 {
            Ok(Self(value))
        } else {
            Err(TryFromIntError(()))
        }
    }
}

impl TryFrom<usize> for u31 {
    type Error = TryFromIntError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        const MASK: usize = !(u31::MAX.0 as usize);

        if value & MASK == 0 {
            Ok(Self(value as u32))
        } else {
            Err(TryFromIntError(()))
        }
    }
}

impl TryFrom<i32> for u31 {
    type Error = TryFromIntError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value >= 0 {
            Ok(Self(value as u32))
        } else {
            Err(TryFromIntError(()))
        }
    }
}

impl From<u31> for u32 {
    fn from(value: u31) -> Self {
        value.0
    }
}

impl From<u31> for i32 {
    fn from(value: u31) -> Self {
        value.0 as i32
    }
}

impl From<u31> for usize {
    fn from(value: u31) -> Self {
        value.0 as usize
    }
}
