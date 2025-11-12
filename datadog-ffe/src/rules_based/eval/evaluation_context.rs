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
    /// done to allow returning subject key as an attribute when rule references "id".
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
    /// If attribute `name` is `"id"` and there's no explicit attribute with this name, return
    /// subject key instead.
    pub fn get_attribute(&self, name: &str) -> Option<&Attribute> {
        let value = self.attributes.get(name);
        if value.is_some() {
            return value;
        }

        if name == "id" {
            return self.targeting_key.as_ref();
        }

        None
    }
}

#[cfg(feature = "pyo3")]
mod pyo3_impl {
    use super::*;

    use pyo3::{intern, prelude::*, types::PyDict};

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
    impl<'py> FromPyObject<'py> for EvaluationContext {
        #[inline]
        fn extract_bound(value: &Bound<'py, PyAny>) -> PyResult<Self> {
            let py = value.py();

            let (targeting_key, attributes) = if let Ok(dict) = value.downcast::<PyDict>() {
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
