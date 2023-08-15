// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use super::super::{pprof, MappingId};
use super::{small_non_zero_pprof_id, Id, Item, Line, PprofItem};
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

    fn from_offset(offset: usize) -> Self {
        Self(small_non_zero_pprof_id(offset).expect("LocationId to fit into a u32"))
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0.get() as u64
    }
}
