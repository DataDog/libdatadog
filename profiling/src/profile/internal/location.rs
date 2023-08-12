// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use super::super::{pprof, MappingId};
use super::{Id, Item, Line, PprofItem};
use std::fmt::Debug;
use std::num::NonZeroU32;

#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Location {
    pub mapping_id: MappingId,
    pub address: u64,
    pub lines: Vec<Line>,
    pub is_folded: bool,
}

impl Item for Location {
    type Id = LocationId;
}

impl PprofItem for Location {
    type PprofMessage = pprof::Location;

    fn to_pprof(&self, id: Self::Id) -> Self::PprofMessage {
        pprof::Location {
            id: id.to_raw_id(),
            mapping_id: self.mapping_id.to_raw_id(),
            address: self.address,
            lines: self.lines.iter().map(pprof::Line::from).collect(),
            is_folded: self.is_folded,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LocationId(NonZeroU32);

impl Id for LocationId {
    type RawId = u64;

    fn from_offset(v: usize) -> Self {
        let index: u32 = v.try_into().expect("LocationId to fit into a u32");

        // PProf reserves location 0.
        // Both this, and the serialization of the table, add 1 to avoid the 0 element
        let index = index.checked_add(1).expect("LocationId to fit into a u32");
        // Safety: the `checked_add(1).expect(...)` guards this from ever being zero.
        let index = unsafe { NonZeroU32::new_unchecked(index) };
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0.get() as u64
    }
}
