// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use datadog_profiling_protobuf::StringOffset;

pub type StringId = StringOffset;

impl Id for StringId {
    type RawId = i64;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        Self::try_from(inner).expect("StringId to fit into a u32")
    }

    fn to_raw_id(&self) -> Self::RawId {
        Self::RawId::from(self)
    }
}
