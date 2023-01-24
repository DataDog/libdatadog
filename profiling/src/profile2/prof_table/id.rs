// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Id(u64);

impl From<Id> for u64 {
    fn from(x: Id) -> Self {
        x.0
    }
}

impl From<u64> for Id {
    fn from(x: u64) -> Self {
        Self(x)
    }
}

#[cfg(target_pointer_width = "64")]
impl From<Id> for usize {
    fn from(value: Id) -> usize {
        value.0 as usize
    }
}

#[cfg(target_pointer_width = "64")]
impl From<usize> for Id {
    fn from(value: usize) -> Self {
        Self { 0: value as u64 }
    }
}
