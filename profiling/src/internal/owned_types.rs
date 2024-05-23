// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;

#[derive(Clone)]
#[cfg_attr(test, derive(bolero_generator::TypeGenerator))]
pub struct ValueType {
    pub typ: Box<str>,
    pub unit: Box<str>,
}

impl<'a> From<&'a api::ValueType<'a>> for ValueType {
    #[inline]
    fn from(value_type: &'a api::ValueType<'a>) -> Self {
        Self {
            typ: String::from(value_type.r#type).into(),
            unit: String::from(value_type.unit).into(),
        }
    }
}

#[derive(Clone)]
pub struct Period {
    pub typ: ValueType,
    pub value: i64,
}

impl<'a> From<&'a api::Period<'a>> for Period {
    #[inline]
    fn from(period: &'a api::Period<'a>) -> Self {
        Self {
            typ: ValueType::from(&period.r#type),
            value: period.value,
        }
    }
}
