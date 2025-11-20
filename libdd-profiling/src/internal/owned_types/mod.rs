// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;

#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
#[derive(Clone, Debug)]
pub struct ValueType {
    pub typ: Box<str>,
    pub unit: Box<str>,
}

impl<'a> From<&'a api::ValueType<'a>> for ValueType {
    #[inline]
    fn from(value_type: &'a api::ValueType<'a>) -> Self {
        Self {
            typ: Box::from(value_type.r#type),
            unit: Box::from(value_type.unit),
        }
    }
}

impl<'a> From<&'a ValueType> for api::ValueType<'a> {
    fn from(value: &'a ValueType) -> Self {
        Self::new(&value.typ, &value.unit)
    }
}

#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
#[derive(Clone, Debug)]
pub struct Period {
    pub typ: ValueType,
    pub value: i64,
}

impl<'a> From<api::Period<'a>> for Period {
    #[inline]
    fn from(period: api::Period<'a>) -> Self {
        Period::from(&period)
    }
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
