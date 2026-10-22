// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// An OpenTelemetry attribute array represented without depending on the OpenTelemetry SDK.
#[derive(Clone, Debug)]
pub enum AttributeArray {
    Bool(Vec<bool>),
    I64(Vec<i64>),
    F64(Vec<f64>),
    String(Vec<String>),
}

impl PartialEq for AttributeArray {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::I64(left), Self::I64(right)) => left == right,
            (Self::F64(left), Self::F64(right)) => left
                .iter()
                .map(|value| value.to_bits())
                .eq(right.iter().map(|value| value.to_bits())),
            (Self::String(left), Self::String(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for AttributeArray {}

impl Hash for AttributeArray {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(values) => values.hash(state),
            Self::I64(values) => values.hash(state),
            Self::F64(values) => values.iter().for_each(|value| value.to_bits().hash(state)),
            Self::String(values) => values.hash(state),
        }
    }
}

/// A scalar or homogeneous-array OpenTelemetry attribute value.
#[derive(Clone, Debug)]
pub enum AttributeValue {
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Array(AttributeArray),
}

impl PartialEq for AttributeValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::I64(left), Self::I64(right)) => left == right,
            (Self::F64(left), Self::F64(right)) => left.to_bits() == right.to_bits(),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Array(left), Self::Array(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for AttributeValue {}

impl Hash for AttributeValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(value) => value.hash(state),
            Self::I64(value) => value.hash(state),
            Self::F64(value) => value.to_bits().hash(state),
            Self::String(value) => value.hash(state),
            Self::Array(value) => value.hash(state),
        }
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for AttributeValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<f64> for AttributeValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<AttributeArray> for AttributeValue {
    fn from(value: AttributeArray) -> Self {
        Self::Array(value)
    }
}

/// A typed OpenTelemetry key/value attribute.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyValue {
    pub key: String,
    pub value: AttributeValue,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

pub(crate) fn normalize_attributes(attributes: &[KeyValue]) -> Vec<KeyValue> {
    let mut by_key = HashMap::with_capacity(attributes.len());
    for attribute in attributes {
        by_key.insert(attribute.key.clone(), attribute.clone());
    }
    let mut normalized = by_key.into_values().collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.key.cmp(&right.key));
    normalized
}

#[cfg(test)]
mod tests {
    use super::{AttributeArray, AttributeValue, KeyValue, normalize_attributes};

    #[test]
    fn float_attributes_have_stable_equality() {
        assert_eq!(AttributeValue::F64(f64::NAN), AttributeValue::F64(f64::NAN));
        assert_ne!(AttributeValue::F64(0.0), AttributeValue::F64(-0.0));
        assert_eq!(
            AttributeArray::F64(vec![1.0, f64::NAN]),
            AttributeArray::F64(vec![1.0, f64::NAN])
        );
    }

    #[test]
    fn attributes_are_sorted_and_deduplicated() {
        assert_eq!(
            normalize_attributes(&[
                KeyValue::new("z", "last"),
                KeyValue::new("a", "first"),
                KeyValue::new("a", "duplicate"),
            ]),
            vec![KeyValue::new("a", "duplicate"), KeyValue::new("z", "last")]
        );
    }
}
