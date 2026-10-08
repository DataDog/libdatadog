// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[derive(Eq, PartialEq, Hash)]
pub struct StackTrace {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id\[0\].
    pub locations: Vec<LocationId>,
}

impl Item for StackTrace {
    type Id = StackTraceId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(C)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct StackTraceId(u32);

impl Id for StackTraceId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        let index: u32 = inner.try_into().expect("StackTraceId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}

impl From<StackTraceId> for u32 {
    fn from(value: StackTraceId) -> Self {
        value.0
    }
}
