use std::collections::HashMap;
use std::sync::Arc;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

use crate::rules_based::{Str, events::AssignmentEvent};

use super::VariationType;

/// Reason for assignment evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssignmentReason {
    /// Assignment was made based on targeting rules or time bounds.
    TargetingMatch,
    /// Assignment was made based on traffic split allocation.
    Split,
    /// Assignment was made as a static/default value.
    Static,
}

impl std::fmt::Display for AssignmentReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssignmentReason::TargetingMatch => write!(f, "TARGETING_MATCH"),
            AssignmentReason::Split => write!(f, "SPLIT"),
            AssignmentReason::Static => write!(f, "STATIC"),
        }
    }
}

/// Result of assignment evaluation.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    /// Assignment value that should be returned to the user.
    pub value: AssignmentValue,
    /// The variation key that was selected for this assignment.
    pub variation_key: Str,
    /// The allocation key that was matched for this assignment.
    pub allocation_key: Str,
    /// Extra logging information for this assignment.
    pub extra_logging: Arc<HashMap<String, String>>,
    /// The reason for this assignment.
    pub reason: AssignmentReason,
    /// Optional assignment event that should be logged to storage.
    pub event: Option<AssignmentEvent>,
}

/// Enum representing values assigned to a subject as a result of feature flag evaluation.
///
/// # Serialization
///
/// When serialized to JSON, serialized as a two-field object with `type` and `value`. Type is one
/// of "STRING", "INTEGER", "NUMERIC", "BOOLEAN", or "JSON". Value is either string, number,
/// boolean, or arbitrary JSON value.
///
/// Example:
/// ```json
/// {"type":"JSON","value":{"hello":"world"}}
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum AssignmentValue {
    /// A string value.
    String(Str),
    /// An integer value.
    Integer(i64),
    /// A numeric value (floating-point).
    Numeric(f64),
    /// A boolean value.
    Boolean(bool),
    /// Arbitrary JSON value.
    Json(Arc<serde_json::Value>),
}

impl Serialize for AssignmentValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AssignmentValue", 2)?;
        match self {
            AssignmentValue::String(s) => {
                state.serialize_field("type", "STRING")?;
                state.serialize_field("value", s)?;
            }
            AssignmentValue::Integer(i) => {
                state.serialize_field("type", "INTEGER")?;
                state.serialize_field("value", i)?;
            }
            AssignmentValue::Numeric(n) => {
                state.serialize_field("type", "NUMERIC")?;
                state.serialize_field("value", n)?;
            }
            AssignmentValue::Boolean(b) => {
                state.serialize_field("type", "BOOLEAN")?;
                state.serialize_field("value", b)?;
            }
            AssignmentValue::Json(value) => {
                state.serialize_field("type", "JSON")?;
                state.serialize_field("value", &**value)?;
            }
        }
        state.end()
    }
}

impl AssignmentValue {
    pub(crate) fn from_wire(
        ty: VariationType,
        value: serde_json::Value,
    ) -> Option<AssignmentValue> {
        use serde_json::Value;
        Some(match (ty, value) {
            (VariationType::String, Value::String(s)) => AssignmentValue::String(s.into()),
            (VariationType::Integer, Value::Number(n)) => AssignmentValue::Integer(n.as_i64()?),
            (VariationType::Numeric, Value::Number(n)) => AssignmentValue::Numeric(n.as_f64()?),
            (VariationType::Boolean, Value::Bool(v)) => AssignmentValue::Boolean(v),
            (VariationType::Json, v) => AssignmentValue::Json(Arc::new(v)),
            // Type mismatch
            _ => return None,
        })
    }

    /// Checks if the assignment value is of type String.
    ///
    /// # Returns
    /// - `true` if the value is of type String, otherwise `false`.
    ///
    /// # Examples
    /// ```
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
    /// let value = AssignmentValue::Numeric(3.14);
    /// assert_eq!(value.is_numeric(), true);
    /// ```
    pub fn is_numeric(&self) -> bool {
        self.as_numeric().is_some()
    }
    /// Returns the assignment value as a numeric value if it is of type Numeric.
    ///
    /// # Returns
    /// - The numeric value if the assignment value is of type Numeric, otherwise `None`.
    ///
    /// # Examples
    /// ```
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
    /// let value = AssignmentValue::Numeric(3.14);
    /// assert_eq!(value.as_numeric(), Some(3.14));
    /// ```
    pub fn as_numeric(&self) -> Option<f64> {
        match self {
            Self::Numeric(n) => Some(*n),
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
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
            Self::Json(value) => Some(value),
            _ => None,
        }
    }
    /// Extracts the assignment value as a JSON value if it is of type Json.
    ///
    /// # Returns
    /// - The JSON value if the assignment value is of type Json, otherwise `None`.
    pub fn to_json(self) -> Option<Arc<serde_json::Value>> {
        match self {
            Self::Json(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the type of the variation as a string.
    ///
    /// # Returns
    /// - A string representing the type of the variation ("STRING", "INTEGER", "NUMERIC", "BOOLEAN", or "JSON").
    ///
    /// # Examples
    /// ```
    /// # use ffe_evaluation::rules_based::ufc::AssignmentValue;
    /// # use ffe_evaluation::rules_based::ufc::VariationType;
    /// let value = AssignmentValue::String("example".into());
    /// assert_eq!(value.variation_type(), VariationType::String);
    /// ```
    pub fn variation_type(&self) -> VariationType {
        match self {
            AssignmentValue::String(_) => VariationType::String,
            AssignmentValue::Integer(_) => VariationType::Integer,
            AssignmentValue::Numeric(_) => VariationType::Numeric,
            AssignmentValue::Boolean(_) => VariationType::Boolean,
            AssignmentValue::Json(_) => VariationType::Json,
        }
    }

    /// Returns the raw value of the variation.
    ///
    /// # Returns
    /// - A JSON Value containing the variation value.
    pub(crate) fn variation_value(&self) -> serde_json::Value {
        use serde_json::{Number, Value};
        match self {
            AssignmentValue::String(s) => Value::String(s.to_string()),
            AssignmentValue::Integer(i) => Value::Number((*i).into()),
            AssignmentValue::Numeric(n) => {
                Value::Number(Number::from_f64(*n).expect("value should not be infinite/NaN"))
            }
            AssignmentValue::Boolean(b) => Value::Bool(*b),
            AssignmentValue::Json(value) => value.as_ref().clone(),
        }
    }
}
