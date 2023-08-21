// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::super::*;

/// A canonical representation for sets of labels.
/// You should only use the impl functions to modify this.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LabelSet {
    sorted_labels: Box<[LabelId]>,
}

impl LabelSet {
    pub fn iter(&self) -> core::slice::Iter<'_, LabelId> {
        self.sorted_labels.iter()
    }

    pub fn new(mut v: Vec<LabelId>) -> Self {
        v.sort_unstable();
        let sorted_labels = v.into_boxed_slice();
        Self { sorted_labels }
    }
}

impl Item for LabelSet {
    type Id = LabelSetId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct LabelSetId(u32);

impl Id for LabelSetId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        let index: u32 = inner.try_into().expect("LabelSetId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}

impl LabelSetId {
    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}
