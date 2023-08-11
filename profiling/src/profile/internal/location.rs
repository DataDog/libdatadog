// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub use super::super::pprof;
use super::Line;
use crate::profile::MappingId;
use std::fmt::Debug;
use std::num::NonZeroU32;

#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Location {
    pub mapping_id: MappingId,
    pub address: u64,
    pub lines: Vec<Line>,
    pub is_folded: bool,
}

impl Location {
    pub fn to_pprof(&self, id: u64) -> pprof::Location {
        pprof::Location {
            id,
            mapping_id: self.mapping_id.into(),
            address: self.address,
            lines: self.lines.iter().map(pprof::Line::from).collect(),
            is_folded: self.is_folded,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LocationId(NonZeroU32);

impl LocationId {
    pub fn new<T>(v: T) -> Self
    where
        T: TryInto<u32>,
        T::Error: Debug,
    {
        let index: u32 = v.try_into().expect("LocationId to fit into a u32");

        // PProf reserves location 0.
        // Both this, and the serialization of the table, add 1 to avoid the 0 element
        let index = index.checked_add(1).expect("LocationId to fit into a u32");
        // Safety: the `checked_add(1).expect(...)` guards this from ever being zero.
        let index = unsafe { NonZeroU32::new_unchecked(index) };
        Self(index)
    }
}

impl From<LocationId> for u64 {
    fn from(s: LocationId) -> Self {
        Self::from(&s)
    }
}

impl From<&LocationId> for u64 {
    fn from(s: &LocationId) -> Self {
        s.0.get().into()
    }
}
