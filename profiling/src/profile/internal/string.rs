// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::Id;

#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct StringId(u32);

impl StringId {
    #[inline]
    pub const fn zero() -> Self {
        Self(0)
    }

    // todo: remove when upscaling uses internal::* instead of pprof::*
    pub fn new<T>(v: T) -> Self
    where
        T: TryInto<u32>,
        T::Error: core::fmt::Debug,
    {
        Self(v.try_into().expect("StringId to fit into a u32"))
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
        Self::new(inner)
    }

    fn to_raw_id(&self) -> Self::RawId {
        Self::RawId::from(self.0)
    }
}
