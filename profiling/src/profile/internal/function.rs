// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use super::super::{pprof, StringId};
use super::{Id, Item, PprofItem};
use std::fmt::Debug;
use std::num::NonZeroU32;

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

    fn from_offset(v: usize) -> Self {
        let index: u32 = v.try_into().expect("FunctionId to fit into a u32");

        // PProf reserves function 0.
        // Both this, and the serialization of the table, add 1 to avoid the 0 element
        let index = index.checked_add(1).expect("FunctionId to fit into a u32");
        // Safety: the `checked_add(1).expect(...)` guards this from ever being zero.
        let index = unsafe { NonZeroU32::new_unchecked(index) };
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0.get().into()
    }
}
