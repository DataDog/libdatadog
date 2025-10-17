// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringId2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ValueType2 {
    pub type_id: StringId2,
    pub unit_id: StringId2,
}

impl ValueType2 {
    pub fn new(type_id: StringId2, unit_id: StringId2) -> ValueType2 {
        ValueType2 { type_id, unit_id }
    }
}
