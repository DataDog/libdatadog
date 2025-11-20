// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringRef;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ValueType {
    pub type_id: StringRef,
    pub unit_id: StringRef,
}

impl ValueType {
    pub fn new(type_id: StringRef, unit_id: StringRef) -> ValueType {
        ValueType { type_id, unit_id }
    }
}
