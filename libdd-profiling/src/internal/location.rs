// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

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
