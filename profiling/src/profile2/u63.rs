// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::fmt::{Display, Formatter};

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

    /// # Panics
    /// Panics if the value is larger than i64::MAX.
    pub fn new(value: u64) -> Self {
        let signed = i64::try_from(value).unwrap();
        Self(signed as u64)
    }
}

impl From<u64> for u63 {
    /// # Panics
    /// Panics if the value is larger than i64::MAX. Since this is unexpected
    /// for our use-case, we opt to panic instead of use try_from.
    fn from(value: u64) -> Self {
        Self::new(value)
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

#[cfg(target_pointer_width = "64")]
impl From<u63> for usize {
    fn from(value: u63) -> usize {
        value.0 as usize
    }
}

#[cfg(target_pointer_width = "64")]
impl From<usize> for u63 {
    /// # Panics
    /// Panics if the value is larger than i64::MAX. Since this is unexpected
    /// for our use-case, we opt to panic instead of use try_from.
    fn from(value: usize) -> Self {
        // Panic: this won't happen on target platforms, hence the cfg guard.
        let value = u64::try_from(value).unwrap();

        // Panic: this might panic though, if it's beyond i64::MAX.
        Self::new(value)
    }
}
