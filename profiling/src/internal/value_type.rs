// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::collections::LengthPrefixedStr;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ValueType {
    pub r#type: (Option<LengthPrefixedStr>, StringId),
    pub unit: (Option<LengthPrefixedStr>, StringId),
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
