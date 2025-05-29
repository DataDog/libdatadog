// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use datadog_profiling_protobuf::{prost_impls, Record, StringOffset};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum LabelValue {
    Str(StringId),
    Num { num: i64, num_unit: StringId },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Label {
    key: StringId,
    value: LabelValue,
}

impl Label {
    pub fn has_num_value(&self) -> bool {
        matches!(self.value, LabelValue::Num { .. })
    }

    pub fn has_string_value(&self) -> bool {
        matches!(self.value, LabelValue::Str(_))
    }

    pub fn get_key(&self) -> StringId {
        self.key
    }

    pub fn get_value(&self) -> &LabelValue {
        &self.value
    }

    pub fn num(key: StringId, num: i64, num_unit: StringId) -> Self {
        Self {
            key,
            value: LabelValue::Num { num, num_unit },
        }
    }

    pub fn str(key: StringId, v: StringId) -> Self {
        Self {
            key,
            value: LabelValue::Str(v),
        }
    }
}

impl From<Label> for prost_impls::Label {
    fn from(l: Label) -> Self {
        Self::from(&l)
    }
}

impl From<&Label> for prost_impls::Label {
    fn from(l: &Label) -> prost_impls::Label {
        let key = l.key.to_raw_id();
        match l.value {
            LabelValue::Str(str) => Self {
                key,
                str: str.to_raw_id(),
                num: 0,
                num_unit: 0,
            },
            LabelValue::Num { num, num_unit } => Self {
                key,
                str: 0,
                num,
                num_unit: num_unit.into_raw_id(),
            },
        }
    }
}

impl From<Label> for datadog_profiling_protobuf::Label {
    fn from(label: Label) -> Self {
        let (str, num, num_unit) = match label.value {
            LabelValue::Str(str) => (str, 0, StringOffset::ZERO),
            LabelValue::Num { num, num_unit } => (StringOffset::ZERO, num, num_unit),
        };
        Self {
            key: Record::from(label.key),
            str: Record::from(str),
            num: Record::from(num),
            num_unit: Record::from(num_unit),
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
    labels: Box<[LabelId]>,
}

impl LabelSet {
    pub fn iter(&self) -> core::slice::Iter<'_, LabelId> {
        self.labels.iter()
    }

    pub fn new(labels: Box<[LabelId]>) -> Self {
        // Once upon a time label ids were guaranteed to be sorted. However,
        // this makes testing difficult because the order of input labels and
        // output labels can make a difference.
        // Unless there is some reason lost to time, we do not need to sort
        // these. Save some cycles, and if a given language increases memory,
        // then it means they aren't adding labels in the same order every
        // time, and they should examine that--but it shouldn't be a
        // correctness issue, as far as I know.
        Self { labels }
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
