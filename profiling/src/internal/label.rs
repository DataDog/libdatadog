// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Label {
    key: StringId,
    str: StringId,
    num: i64,
    num_unit: StringId,
}

impl Label {
    #[inline]
    pub fn get_key(&self) -> StringId {
        self.key
    }

    #[inline]
    pub fn get_str(&self) -> StringId {
        self.str
    }

    #[inline]
    pub fn get_num(&self) -> (i64, StringId) {
        (self.num, self.num_unit)
    }

    pub fn num(key: StringId, num: i64, num_unit: StringId) -> Self {
        Self {
            key,
            str: StringId::ZERO,
            num,
            num_unit,
        }
    }

    pub fn str(key: StringId, str: StringId) -> Self {
        Self {
            key,
            str,
            num: 0,
            num_unit: StringId::ZERO,
        }
    }
}

impl From<Label> for pprof::Label {
    fn from(l: Label) -> Self {
        Self::from(&l)
    }
}

impl From<&Label> for pprof::Label {
    fn from(l: &Label) -> pprof::Label {
        let key = l.key.to_raw_id();
        Self {
            key,
            str: l.str.to_raw_id(),
            num: l.num,
            num_unit: l.num_unit.to_raw_id(),
        }
    }
}

impl Item for Label {
    type Id = LabelId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(C)]
pub struct LabelId(u32);

impl Id for LabelId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        let index: u32 = inner.try_into().expect("LabelId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}
impl LabelId {
    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}

/// A canonical representation for sets of labels.
/// You should only use the impl functions to modify this.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LabelSet {
    // Guaranteed to be sorted by [Self::new]
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
#[repr(C)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct LabelSetId(u32);

impl Id for LabelSetId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
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

impl From<LabelSetId> for u32 {
    fn from(value: LabelSetId) -> Self {
        value.0
    }
}
