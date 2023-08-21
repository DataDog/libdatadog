// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::super::*;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum LabelValue {
    Str(StringId),
    Num {
        num: i64,
        num_unit: Option<StringId>,
    },
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

    pub fn num(key: StringId, num: i64, num_unit: Option<StringId>) -> Self {
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

impl From<Label> for pprof::Label {
    fn from(l: Label) -> Self {
        Self::from(&l)
    }
}

impl From<&Label> for pprof::Label {
    fn from(l: &Label) -> pprof::Label {
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
                num_unit: num_unit.map(StringId::into_raw_id).unwrap_or_default(),
            },
        }
    }
}

impl Item for Label {
    type Id = LabelId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LabelId(u32);

impl Id for LabelId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        let index: u32 = inner.try_into().expect("LabelId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}
