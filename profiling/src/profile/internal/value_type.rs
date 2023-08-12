// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::super::{pprof, Id, StringId};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ValueType {
    pub r#type: StringId,
    pub unit: StringId,
}

impl From<ValueType> for pprof::ValueType {
    fn from(vt: ValueType) -> Self {
        Self::from(&vt)
    }
}

impl From<&ValueType> for pprof::ValueType {
    fn from(vt: &ValueType) -> Self {
        Self {
            r#type: vt.r#type.to_raw_id(),
            unit: vt.unit.to_raw_id(),
        }
    }
}
