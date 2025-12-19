// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Represents a \[pprof::Location\] with some space-saving changes:
///  - The id is not stored on the struct. It's stored in the container that holds the struct.
///  - ids for linked objects use 32-bit numbers instead of 64 bit ones.
///  - in libdatadog, we always use 1 Line per Location, so this is directly inlined into the
///    struct.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Location {
    pub mapping_id: Option<MappingId>,
    pub function_id: FunctionId,
    pub address: u64,
    pub line: i64,
}

impl Item for Location {
    type Id = LocationId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(C)]
pub struct LocationId(NonZeroU32);

impl Id for LocationId {
    type RawId = u64;

    fn from_offset(offset: usize) -> Self {
        #[allow(clippy::expect_used)]
        Self(small_non_zero_pprof_id(offset).expect("LocationId to fit into a u32"))
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0.get().into()
    }
}
