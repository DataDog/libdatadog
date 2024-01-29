// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::*;

#[derive(Eq, PartialEq, Hash)]
pub struct StackTrace {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id[0].
    pub locations: Vec<LocationId>,
}

impl Item for StackTrace {
    type Id = StackTraceId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct StackTraceId(u32);

impl Id for StackTraceId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        let index: u32 = inner.try_into().expect("StackTraceId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}
