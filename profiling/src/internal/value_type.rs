// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::*;
use crate::collections::LengthPrefixedStr;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ValueType {
    pub r#type: (LengthPrefixedStr, StringId),
    pub unit: (LengthPrefixedStr, StringId),
}

impl From<ValueType> for pprof::ValueType {
    fn from(vt: ValueType) -> Self {
        Self::from(&vt)
    }
}

impl From<&ValueType> for pprof::ValueType {
    fn from(vt: &ValueType) -> Self {
        Self {
            r#type: vt.r#type.1.to_raw_id(),
            unit: vt.unit.1.to_raw_id(),
        }
    }
}
