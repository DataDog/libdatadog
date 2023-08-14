// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::*;

/// Represents a [pprof::Location] with some space-saving changes:
///  - The id is not stored on the struct. It's stored in the container that
///    holds the struct.
///  - ids for linked objects use 32-bit numbers instead of 64 bit ones.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Location {
    pub mapping_id: MappingId,
    pub function_id: FunctionId,
    pub address: u64,
    pub line: i64,
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
            lines: vec![pprof::Line {
                function_id: self.function_id.to_raw_id(),
                line: self.line,
            }],
            is_folded: false,
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
        self.0.get().into()
    }
}
