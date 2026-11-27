// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use crate::rules_based::Str;

use crate::rules_based::Attribute;

/// `Subject` is a bundle of subject attributes and a key.
#[derive(Debug)]
pub struct EvaluationContext {
    /// Subject key encoded as attribute value. Known to be `AttributeValue::String`. This is
    /// done to allow returning subject key as an attribute when rule references "id" or
    /// "targetingKey".
    targeting_key: Option<Attribute>,
    attributes: Arc<HashMap<Str, Attribute>>,
}

impl EvaluationContext {
    pub fn new(key: Option<Str>, attributes: Arc<HashMap<Str, Attribute>>) -> EvaluationContext {
        EvaluationContext {
            targeting_key: key.map(Attribute::from),
            attributes,
        }
    }

    pub fn targeting_key(&self) -> Option<&Str> {
        self.targeting_key.as_ref().and_then(|it| it.as_str())
    }

    /// Get subject attribute.
    ///
    /// If attribute `name` is `"id"` or `"targetingKey"` and there's no explicit attribute with
    /// this name, return subject key instead.
    pub fn get_attribute(&self, name: &str) -> Option<&Attribute> {
        let value = self.attributes.get(name);
        if value.is_some() {
            return value;
        }

        if falls_back_to_targeting_key(name) {
            return self.targeting_key.as_ref();
        }

        None
    }
}

/// Attributes that resolve to the targeting key when the context has no explicit attribute with
/// this name.
fn falls_back_to_targeting_key(name: &str) -> bool {
    name == "id" || name == "targetingKey"
}

#[cfg(feature = "pyo3")]
mod pyo3_impl {
    use super::*;

    use pyo3::{Borrowed, intern, prelude::*, types::PyDict};

    /// Accepts either a dict with `"targeting_key"` and `"attributes"` items, or any object with
    /// `targeting_key` and `attributes` attributes.
    ///
    /// # Examples
    ///
    /// ```python
    /// {"targeting_key": "user1", "attributes": {"attr1": 42}}
    /// ```
    ///
    /// ```python
    /// @dataclass
    /// class EvaluationContext:
    ///     targeting_key: Optional[str]
    ///     attributes: dict[str, Any]
    ///
    /// EvaluationContext(targeting_key="user1", attributes={"attr1": 42})
    /// ```
    impl<'a, 'py> FromPyObject<'a, 'py> for EvaluationContext {
        type Error = PyErr;

        #[inline]
        fn extract(value: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
            let py = value.py();

            let (targeting_key, attributes) = if let Ok(dict) = value.cast::<PyDict>() {
                (
                    dict.get_item(intern!(py, "targeting_key"))?,
                    dict.get_item(intern!(py, "attributes"))?,
                )
            } else {
                (
                    value.getattr_opt(intern!(py, "targeting_key"))?,
                    value.getattr_opt(intern!(py, "attributes"))?,
                )
            };

            let context = EvaluationContext::new(
                targeting_key
                    .map(|it| it.extract())
                    .transpose()?
                    .unwrap_or(None),
                attributes
                    .map(|it| it.extract())
                    .transpose()?
                    .map(Arc::new)
                    .unwrap_or_default(),
            );

            Ok(context)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(key: Option<&str>, attributes: &[(&str, &str)]) -> EvaluationContext {
        EvaluationContext::new(
            key.map(Str::from),
            Arc::new(
                attributes
                    .iter()
                    .map(|(name, value)| (Str::from(*name), Attribute::from(*value)))
                    .collect(),
            ),
        )
    }

    fn attribute<'a>(context: &'a EvaluationContext, name: &str) -> Option<&'a str> {
        context
            .get_attribute(name)
            .and_then(|it| it.as_str())
            .map(|it| it.as_str())
    }

    #[test]
    fn targeting_key_attribute_falls_back_to_targeting_key() {
        let context = context(Some("user-1"), &[]);
        assert_eq!(attribute(&context, "targetingKey"), Some("user-1"));
        assert_eq!(attribute(&context, "id"), Some("user-1"));
    }

    #[test]
    fn explicit_attribute_wins_over_targeting_key() {
        let context = context(Some("user-1"), &[("targetingKey", "other"), ("id", "x")]);
        assert_eq!(attribute(&context, "targetingKey"), Some("other"));
        assert_eq!(attribute(&context, "id"), Some("x"));
    }

    #[test]
    fn missing_targeting_key_is_missing_attribute() {
        let context = context(None, &[]);
        assert!(context.get_attribute("targetingKey").is_none());
        assert!(context.get_attribute("id").is_none());
    }

    #[test]
    fn other_attributes_do_not_fall_back() {
        let context = context(Some("user-1"), &[]);
        assert!(context.get_attribute("targeting_key").is_none());
        assert!(context.get_attribute("key").is_none());
    }
}
