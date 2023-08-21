// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::super::*;

/// A canonical representation for sets of labels.
/// You should only use the impl functions to modify this.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LabelSet {
    sorted_labels: Vec<LabelId>,
}

impl LabelSet {
    pub fn new() -> Self {
        Self {
            sorted_labels: vec![],
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            sorted_labels: Vec::with_capacity(capacity),
        }
    }

    pub fn from_vec(mut v: Vec<LabelId>) -> Self {
        v.sort_unstable();
        Self { sorted_labels: v }
    }

    pub fn iter(&self) -> core::slice::Iter<'_, LabelId> {
        self.sorted_labels.iter()
    }

    pub fn add(&mut self, l: LabelId) {
        self.sorted_labels.push(l);
        self.sorted_labels.sort_unstable();
    }

    pub fn extend(&mut self, ls: &[LabelId]) {
        self.sorted_labels.extend(ls);
        self.sorted_labels.sort_unstable();
    }
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
