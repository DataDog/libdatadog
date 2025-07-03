// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringId;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ValueType {
    pub type_id: StringId,
    pub unit_id: StringId,
}

impl ValueType {
    pub fn new(type_id: StringId, unit_id: StringId) -> ValueType {
        ValueType { type_id, unit_id }
    }
}
