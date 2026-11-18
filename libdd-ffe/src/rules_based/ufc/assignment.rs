// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rules_based::{FlagType, Str, ufc::VariationType};

use super::models::Feature;

/// Reason for assignment evaluation result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssignmentReason {
    /// Assignment was made based on targeting rules.
    TargetingMatch,
    /// Assignment was made based on traffic split allocation.
    Split,
    /// Assignment was made from a platform/default allocation value.
    Default,
    /// Assignment was made as a static/default value.
    Static,
}

/// Result of assignment evaluation.
#[derive(Debug, Clone)]
pub struct Assignment {
    /// Assignment value that should be returned to the user.
    pub value: AssignmentValue,
    /// The variation key that was selected for this assignment.
    pub variation_key: Str,
    /// The allocation key that was matched for this assignment.
    pub allocation_key: Str,
    /// The reason for this assignment.
    pub reason: AssignmentReason,
    /// Serial id for the selected split.
    pub serial_id: Option<i32>,
    /// Whether this assignment is part of an experiment and should be logged.
    pub do_log: bool,
    /// Features attached to the selected split, shared with the compiled split. `None` when the
    /// split has none, so an assignment without features costs no reference count update.
    pub features: Option<Arc<[Feature]>>,
}

impl Assignment {
    /// The features of the selected split; empty when it has none.
    pub fn features_slice(&self) -> &[Feature] {
        self.features.as_deref().unwrap_or(&[])
    }
}

/// Enum representing values assigned to a subject as a result of feature flag evaluation.
///
/// # Serialization
///
/// When serialized to JSON, serialized as a two-field object with `type` and `value`. Type is one
/// of "STRING", "INTEGER", "FLOAT", "BOOLEAN", or "JSON". Value is either string, number,
/// boolean, or arbitrary JSON value.
///
/// Example:
/// ```json
/// {"type":"JSON","value":{"hello":"world"}}
/// ```
#[derive(Debug, Clone)]
pub enum AssignmentValue {
    /// A string value.
    String(Str),
    /// An integer value.
    Integer(i64),
    /// A numeric value (floating-point).
    Float(f64),
    /// A boolean value.
    Boolean(bool),
    /// Arbitrary JSON value.
    Json {
        value: Arc<serde_json::Value>,
        raw: Arc<serde_json::value::RawValue>,
        /// Language objects converted from `value`. Clones share it with the variation in the
        /// configuration.
        cache: JsonValueCache,
    },
}

/// Language-binding objects converted from a JSON variation value on first use.
///
/// Every clone of an [`AssignmentValue::Json`] shares one cache with the variation it came from,
/// so each repeat evaluation of that variation returns the same converted object. The cache lives
/// as long as the configuration, or the last assignment cloned from it. A new configuration starts
/// with empty caches.
#[derive(Clone)]
pub struct JsonValueCache {
    /// The `dict`, `list` or scalar built by the `pyo3` binding. Callers must treat it as
    /// read-only: a change is visible to every later evaluation of the variation.
    #[cfg(feature = "pyo3")]
    python: Arc<pyo3::sync::PyOnceLock<pyo3::Py<pyo3::PyAny>>>,
}

// Derivable only without the `pyo3` feature.
#[allow(clippy::derivable_impls)]
impl Default for JsonValueCache {
    fn default() -> Self {
        JsonValueCache {
            #[cfg(feature = "pyo3")]
            python: Arc::new(pyo3::sync::PyOnceLock::new()),
        }
    }
}

impl std::fmt::Debug for JsonValueCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonValueCache").finish_non_exhaustive()
    }
}

impl AssignmentValue {
    pub(crate) fn from_wire(
        ty: VariationType,
        value: Arc<serde_json::value::RawValue>,
    ) -> Option<AssignmentValue> {
        let result = match ty {
            VariationType::String => {
                AssignmentValue::String(serde_json::from_str(value.get()).ok()?)
            }
            VariationType::Integer => {
                AssignmentValue::Integer(serde_json::from_str(value.get()).ok()?)
            }
            VariationType::Numeric => {
                AssignmentValue::Float(serde_json::from_str(value.get()).ok()?)
            }
            VariationType::Boolean => {
                AssignmentValue::Boolean(serde_json::from_str(value.get()).ok()?)
            }
            VariationType::Json => AssignmentValue::Json {
                value: serde_json::from_str(value.get()).ok()?,
                raw: value,
                cache: JsonValueCache::default(),
            },
        };

        Some(result)
    }

    /// Checks if the assignment value is of type String.
    ///
    /// # Returns
    /// - `true` if the value is of type String, otherwise `false`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::String("example".into());
    /// assert_eq!(value.is_string(), true);
    /// ```
    pub fn is_string(&self) -> bool {
        self.as_str().is_some()
    }
    /// Returns the assignment value as a string if it is of type String.
    ///
    /// # Returns
    /// - The string value if the assignment value is of type String, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::String("example".into());
    /// assert_eq!(value.as_str(), Some("example"));
    /// ```
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AssignmentValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Extracts the assignment value as a string if it is of type String.
    ///
    /// # Returns
    /// - The string value if the assignment value is of type String, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::String("example".into());
    /// assert_eq!(value.to_string(), Some("example".into()));
    /// ```
    pub fn to_string(self) -> Option<Str> {
        match self {
            AssignmentValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Checks if the assignment value is of type Integer.
    ///
    /// # Returns
    /// - `true` if the value is of type Integer, otherwise `false`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Integer(42);
    /// assert_eq!(value.is_integer(), true);
    /// ```
    pub fn is_integer(&self) -> bool {
        self.as_integer().is_some()
    }
    /// Returns the assignment value as an integer if it is of type Integer.
    ///
    /// # Returns
    /// - The integer value if the assignment value is of type Integer, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Integer(42);
    /// assert_eq!(value.as_integer(), Some(42));
    /// ```
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            AssignmentValue::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Checks if the assignment value is of type Numeric.
    ///
    /// # Returns
    /// - `true` if the value is of type Numeric, otherwise `false`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Float(3.14);
    /// assert_eq!(value.is_float(), true);
    /// ```
    pub fn is_float(&self) -> bool {
        self.as_float().is_some()
    }
    /// Returns the assignment value as a numeric value if it is of type Numeric.
    ///
    /// # Returns
    /// - The numeric value if the assignment value is of type Numeric, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Float(3.14);
    /// assert_eq!(value.as_float(), Some(3.14));
    /// ```
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(n) => Some(*n),
            _ => None,
        }
    }

    /// Checks if the assignment value is of type Boolean.
    ///
    /// # Returns
    /// - `true` if the value is of type Boolean, otherwise `false`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Boolean(true);
    /// assert_eq!(value.is_boolean(), true);
    /// ```
    pub fn is_boolean(&self) -> bool {
        self.as_boolean().is_some()
    }
    /// Returns the assignment value as a boolean if it is of type Boolean.
    ///
    /// # Returns
    /// - The boolean value if the assignment value is of type Boolean, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::AssignmentValue;
    /// let value = AssignmentValue::Boolean(true);
    /// assert_eq!(value.as_boolean(), Some(true));
    /// ```
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            AssignmentValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Checks if the assignment value is of type Json.
    ///
    /// # Returns
    /// - `true` if the value is of type Json, otherwise `false`.
    pub fn is_json(&self) -> bool {
        self.as_json().is_some()
    }
    /// Returns the assignment value as a JSON value if it is of type Json.
    ///
    /// # Returns
    /// - The JSON value if the assignment value is of type Json, otherwise `None`.
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json { value, .. } => Some(value),
            _ => None,
        }
    }
    /// Extracts the assignment value as a JSON value if it is of type Json.
    ///
    /// # Returns
    /// - The JSON value if the assignment value is of type Json, otherwise `None`.
    pub fn to_json(self) -> Option<Arc<serde_json::Value>> {
        match self {
            Self::Json { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Returns the type of the variation as a string.
    ///
    /// # Returns
    /// - A string representing the type of the variation ("STRING", "INTEGER", "NUMERIC",
    ///   "BOOLEAN", or "JSON").
    ///
    /// # Examples
    /// ```
    /// # use libdd_ffe::rules_based::{AssignmentValue, FlagType};
    /// let value = AssignmentValue::String("example".into());
    /// assert_eq!(value.variation_type(), FlagType::String);
    /// ```
    pub fn variation_type(&self) -> FlagType {
        match self {
            AssignmentValue::String(_) => FlagType::String,
            AssignmentValue::Integer(_) => FlagType::Integer,
            AssignmentValue::Float(_) => FlagType::Float,
            AssignmentValue::Boolean(_) => FlagType::Boolean,
            AssignmentValue::Json { .. } => FlagType::Object,
        }
    }

    /// Returns the raw value of the variation.
    ///
    /// # Returns
    /// - A JSON Value containing the variation value.
    pub fn variation_value(&self) -> serde_json::Value {
        use serde_json::{Number, Value};
        match self {
            AssignmentValue::String(s) => Value::String(s.to_string()),
            AssignmentValue::Integer(i) => Value::Number((*i).into()),
            AssignmentValue::Float(n) => {
                Value::Number(Number::from_f64(*n).expect("value should not be infinite/NaN"))
            }
            AssignmentValue::Boolean(b) => Value::Bool(*b),
            AssignmentValue::Json { value, .. } => value.as_ref().clone(),
        }
    }
}

impl PartialEq<AssignmentValue> for AssignmentValue {
    fn eq(&self, other: &AssignmentValue) -> bool {
        match (self, other) {
            (Self::String(l0), Self::String(r0)) => l0 == r0,
            (Self::Integer(l0), Self::Integer(r0)) => l0 == r0,
            (Self::Float(l0), Self::Float(r0)) => l0 == r0,
            (Self::Boolean(l0), Self::Boolean(r0)) => l0 == r0,
            (Self::Json { value: l_value, .. }, Self::Json { value: r_value, .. }) => {
                l_value == r_value
            }
            _ => false,
        }
    }
}

#[cfg(feature = "pyo3")]
mod pyo3_impl {
    use super::*;

    use crate::rules_based::ufc::models::FeatureValue;

    use pyo3::{
        exceptions::PyValueError,
        prelude::*,
        types::{PyDict, PyList},
    };

    impl<'py> IntoPyObject<'py> for &FeatureValue {
        type Target = PyAny;
        type Output = Bound<'py, PyAny>;
        type Error = PyErr;

        #[inline]
        fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
            let obj = match self {
                FeatureValue::String(v) => v.into_pyobject(py)?.into_any(),
                FeatureValue::Number(v) => v.into_pyobject(py)?.into_any(),
                FeatureValue::Boolean(v) => v.into_pyobject(py)?.to_owned().into_any(),
            };
            Ok(obj)
        }
    }

    impl<'py> IntoPyObject<'py> for &AssignmentValue {
        type Target = PyAny;
        type Output = Bound<'py, PyAny>;
        type Error = PyErr;

        #[inline]
        fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
            let obj = match self {
                AssignmentValue::String(v) => v.into_pyobject(py)?.into_any(),
                AssignmentValue::Integer(v) => v.into_pyobject(py)?.into_any(),
                AssignmentValue::Float(v) => v.into_pyobject(py)?.into_any(),
                AssignmentValue::Boolean(v) => v.into_pyobject(py)?.to_owned().into_any(),
                // Convert once per variation and share the object, rather than building a new
                // tree on every evaluation. `PyOnceLock` detaches from the interpreter while it
                // waits for another thread's conversion, so it cannot deadlock with the GIL, and it
                // is thread-safe on free-threaded builds.
                AssignmentValue::Json { value, cache, .. } => cache
                    .python
                    .get_or_try_init(py, || json_to_pyobject(value, py).map(Bound::unbind))?
                    .bind(py)
                    .clone(),
            };
            Ok(obj)
        }
    }

    fn json_to_pyobject<'py>(
        value: &serde_json::Value,
        py: Python<'py>,
    ) -> Result<Bound<'py, PyAny>, PyErr> {
        use serde_json::Value;
        let v = match value {
            Value::Null => py.None().into_bound(py),
            Value::Bool(v) => v.into_pyobject(py)?.to_owned().into_any(),
            Value::Number(number) => {
                if let Some(v) = number.as_u128() {
                    v.into_pyobject(py)?.into_any()
                } else if let Some(v) = number.as_i128() {
                    v.into_pyobject(py)?.into_any()
                } else if let Some(v) = number.as_f64() {
                    v.into_pyobject(py)?.into_any()
                } else {
                    // NOTE: this can only happen if serde_json is compiled with arbitrary-precision
                    // and it failed to parse the number / the number is larger than f64::MAX.
                    return Err(PyValueError::new_err("unable to convert number to python"));
                }
            }
            Value::String(s) => s.into_pyobject(py)?.into_any(),
            Value::Array(values) => {
                let vals = values
                    .iter()
                    .map(|it| json_to_pyobject(it, py))
                    .collect::<Result<Vec<_>, _>>()?;
                PyList::new(py, vals)?.into_any()
            }
            Value::Object(map) => {
                let dict = PyDict::new(py);
                for (key, value) in map {
                    dict.set_item(key, json_to_pyobject(value, py)?)?;
                }
                dict.into_any()
            }
        };
        Ok(v)
    }
}

#[cfg(all(test, feature = "pyo3"))]
mod pyo3_tests {
    use pyo3::{IntoPyObject, Python, types::PyAnyMethods};

    use crate::rules_based::{
        Configuration, EvaluationContext, UniversalFlagConfig, ufc::AssignmentValue,
    };

    const CONFIG: &str = r#"{
        "createdAt": "2024-04-17T19:40:53.716Z",
        "format": "SERVER",
        "environment": {"name": "test"},
        "flags": {
            "json-flag": {
                "key": "json-flag",
                "enabled": true,
                "variationType": "JSON",
                "variations": {"a": {"key": "a", "value": {"list": [1, 2.5, "x", null, true]}}},
                "allocations": [{"key": "all", "splits": [{"variationKey": "a", "shards": []}], "doLog": false}]
            }
        }
    }"#;

    fn json_value(config: &Configuration) -> AssignmentValue {
        let subject = EvaluationContext::new(Some("subject".into()), Default::default());
        config
            .eval_flag(
                "json-flag",
                &subject,
                crate::ExpectedFlagType::Object,
                crate::rules_based::now(),
            )
            .expect("json-flag should evaluate")
            .value
    }

    fn load() -> Configuration {
        Configuration::from_server_response(
            UniversalFlagConfig::from_json(CONFIG.as_bytes().to_vec()).expect("valid config"),
        )
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn repeat_evaluations_share_one_python_object() {
        Python::initialize();
        Python::attach(|py| {
            let config = load();
            let first = (&json_value(&config)).into_pyobject(py).unwrap();
            let second = (&json_value(&config)).into_pyobject(py).unwrap();
            assert!(first.is(&second), "repeat evaluation built a new object");
            let expected = py
                .import("json")
                .unwrap()
                .call_method1("loads", (r#"{"list": [1, 2.5, "x", null, true]}"#,))
                .unwrap();
            assert!(first.eq(&expected).unwrap());

            // A new configuration starts with an empty cache.
            let reloaded = (&json_value(&load())).into_pyobject(py).unwrap();
            assert!(
                !first.is(&reloaded),
                "a new configuration reused the old object"
            );
            assert!(reloaded.eq(&expected).unwrap());
        });
    }
}
