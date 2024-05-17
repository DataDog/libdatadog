// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api::{Period, ValueType};

#[derive(Clone)]
pub struct OwnedValueType {
    pub typ: Box<str>,
    pub unit: Box<str>,
}

impl<'a> From<&'a ValueType<'a>> for OwnedValueType {
    #[inline]
    fn from(value_type: &'a ValueType<'a>) -> Self {
        Self {
            typ: String::from(value_type.r#type).into(),
            unit: String::from(value_type.unit).into(),
        }
    }
}

#[derive(Clone)]
pub struct OwnedPeriod {
    pub typ: OwnedValueType,
    pub value: i64,
}

impl<'a> From<&'a Period<'a>> for OwnedPeriod {
    #[inline]
    fn from(period: &'a Period<'a>) -> Self {
        Self {
            typ: OwnedValueType::from(&period.r#type),
            value: period.value,
        }
    }
}
