// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;
use crate::api2::{Period2, ValueType2};
use crate::profiles::collections::StringRef;
use std::ops::Deref;

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

impl From<ValueType2> for ValueType {
    fn from(value_type2: ValueType2) -> ValueType {
        let typ: StringRef = value_type2.type_id.into();
        let unit: StringRef = value_type2.unit_id.into();
        ValueType {
            typ: Box::from(typ.0.deref()),
            unit: Box::from(unit.0.deref()),
        }
    }
}
impl From<&ValueType2> for ValueType {
    fn from(value_type2: &ValueType2) -> ValueType {
        ValueType::from(*value_type2)
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

impl<'a> From<&'a api::Period<'a>> for Period {
    #[inline]
    fn from(period: &'a api::Period<'a>) -> Self {
        Self {
            typ: ValueType::from(&period.r#type),
            value: period.value,
        }
    }
}

impl From<Period2> for Period {
    fn from(period2: Period2) -> Period {
        Period {
            typ: ValueType::from(period2.r#type),
            value: 0,
        }
    }
}
