// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

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
