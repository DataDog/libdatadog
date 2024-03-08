// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Represents a [pprof::Function] with some space-saving changes:
///  - The id is not stored on the struct. It's stored in the container that holds the struct.
///  - ids for linked objects use 32-bit numbers instead of 64 bit ones.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Function {
    pub name: StringId,
    pub system_name: StringId,
    pub filename: StringId,
    pub start_line: i64,
}

impl Item for Function {
    type Id = FunctionId;
}

impl PprofItem for Function {
    type PprofMessage = pprof::Function;

    fn to_pprof(&self, id: Self::Id) -> Self::PprofMessage {
        pprof::Function {
            id: id.to_raw_id(),
            name: self.name.to_raw_id(),
            system_name: self.system_name.to_raw_id(),
            filename: self.filename.to_raw_id(),
            start_line: self.start_line,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct FunctionId(NonZeroU32);

impl Id for FunctionId {
    type RawId = u64;

    fn from_offset(offset: usize) -> Self {
        Self(small_non_zero_pprof_id(offset).expect("FunctionId to fit into a u32"))
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0.get().into()
    }
}
