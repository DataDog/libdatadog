// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::hash::Hash;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(test, derive(bolero_generator::TypeGenerator))]
pub struct Sample {
    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: LabelSetId,
    pub stacktrace: StackTraceId,
}

impl Item for Sample {
    type Id = SampleId;
}

impl Sample {
    pub fn new(labels: LabelSetId, stacktrace: StackTraceId) -> Self {
        Self { labels, stacktrace }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct SampleId(u32);

impl SampleId {
    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}

impl Id for SampleId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        let index: u32 = inner.try_into().expect("SampleId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(self) -> Self::RawId {
        self.0 as Self::RawId
    }
}
