// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::fmt::{Display, Formatter};
use std::num::TryFromIntError;

#[allow(non_camel_case_types)]
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct u63(u64);

impl Display for u63 {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl u63 {
    pub const MIN: u63 = u63(0);
    pub const MAX: u63 = u63(i64::MAX as u64);

    pub fn new(value: u64) -> Self {
        u63::try_from(value).unwrap()
    }
}

impl TryFrom<u64> for u63 {
    type Error = TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let converted: i64 = value.try_into()?;
        Ok(Self(converted as u64))
    }
}

impl TryFrom<usize> for u63 {
    type Error = TryFromIntError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let converted: i64 = value.try_into()?;
        Ok(Self(converted as u64))
    }
}

impl From<u63> for u64 {
    fn from(x: u63) -> Self {
        x.0
    }
}

impl From<u63> for i64 {
    fn from(x: u63) -> Self {
        x.0 as i64
    }
}

impl TryFrom<u63> for usize {
    type Error = TryFromIntError;

    fn try_from(value: u63) -> Result<Self, Self::Error> {
        let converted: u64 = value.into();
        converted.try_into()
    }
}
