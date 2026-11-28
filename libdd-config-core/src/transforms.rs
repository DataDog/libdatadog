// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Error, Number, Origin, Value, ValueKind};

#[derive(Clone, Copy, Debug)]
pub struct TransformContext<'a> {
    pub key: &'a str,
    pub origin: Origin,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransformResult {
    Accept {
        value: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    Reject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostic: Option<String>,
    },
}

impl TransformResult {
    #[must_use]
    pub const fn accept(value: Value) -> Self {
        Self::Accept {
            value,
            warning: None,
        }
    }
}

pub type TransformFn = fn(Value, TransformContext<'_>) -> TransformResult;

#[derive(Clone, Copy, Debug)]
pub enum Binding {
    Function(TransformFn),
    Host(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Transform {
    input: ValueKind,
    pub(crate) output: ValueKind,
    pub(crate) binding: Binding,
}

impl Transform {
    #[must_use]
    pub const fn new(input: ValueKind, output: ValueKind, function: TransformFn) -> Self {
        Self {
            input,
            output,
            binding: Binding::Function(function),
        }
    }

    #[must_use]
    pub const fn host(input: ValueKind, output: ValueKind, slot: u32) -> Self {
        Self {
            input,
            output,
            binding: Binding::Host(slot),
        }
    }
}

#[derive(Debug, Default)]
pub struct TransformRegistry {
    bindings: HashMap<String, Vec<Transform>>,
}

impl TransformRegistry {
    #[must_use]
    pub fn with_builtins() -> Self {
        Self {
            bindings: HashMap::from([
                (
                    "toLowerCase".to_owned(),
                    vec![
                        Transform::new(ValueKind::String, ValueKind::String, lower_case),
                        Transform::new(ValueKind::Array, ValueKind::Array, lower_case),
                    ],
                ),
                (
                    "toUpperCase".to_owned(),
                    vec![
                        Transform::new(ValueKind::String, ValueKind::String, upper_case),
                        Transform::new(ValueKind::Array, ValueKind::Array, upper_case),
                    ],
                ),
                (
                    "sampleRate".to_owned(),
                    vec![Transform::new(
                        ValueKind::Decimal,
                        ValueKind::Decimal,
                        sample_rate,
                    )],
                ),
            ]),
        }
    }

    /// Register a callable for one input type under a manifest name.
    ///
    /// # Errors
    /// Returns an error if the name and input kind already have a binding.
    pub fn register(&mut self, name: impl Into<String>, transform: Transform) -> Result<(), Error> {
        let name = name.into();
        let entries = self.bindings.entry(name.clone()).or_default();
        if entries
            .iter()
            .any(|existing| existing.input == transform.input)
        {
            return Err(Error::DuplicateTransform(name));
        }
        entries.push(transform);
        Ok(())
    }

    pub(crate) fn bind(&self, key: &str, label: &str, kind: ValueKind) -> Result<Transform, Error> {
        self.bindings
            .get(label)
            .and_then(|entries| entries.iter().find(|transform| transform.input == kind))
            .copied()
            .ok_or_else(|| Error::UnboundTransform {
                key: key.to_owned(),
                name: label.to_owned(),
            })
    }
}

fn lower_case(value: Value, _: TransformContext<'_>) -> TransformResult {
    ascii_or_unicode_case(value, str::to_lowercase)
}

fn upper_case(value: Value, _: TransformContext<'_>) -> TransformResult {
    ascii_or_unicode_case(value, str::to_uppercase)
}

fn ascii_or_unicode_case(value: Value, convert: fn(&str) -> String) -> TransformResult {
    match value {
        Value::String(value) => TransformResult::accept(Value::String(convert(&value))),
        Value::Array(mut items) => {
            for item in &mut items {
                if let Value::String(value) = item {
                    *value = convert(value);
                }
            }
            TransformResult::accept(Value::Array(items))
        }
        _ => TransformResult::Reject { diagnostic: None },
    }
}

fn sample_rate(value: Value, _: TransformContext<'_>) -> TransformResult {
    let Value::Number(number) = value else {
        return TransformResult::Reject { diagnostic: None };
    };
    let supplied = number.as_float();
    if (0.0..=1.0).contains(&supplied) {
        return TransformResult::accept(value);
    }
    TransformResult::Accept {
        value: Value::Number(Number::Float(supplied.clamp(0.0, 1.0))),
        warning: Some("Sample rate out of range between 0 and 1".to_owned()),
    }
}
