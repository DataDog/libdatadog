use std::{borrow::Cow, collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::rules_based::Str;

/// Type alias for a HashMap representing key-value pairs of attributes.
///
/// Keys are strings representing attribute names.
///
/// # Examples
/// ```
/// # use ffe_evaluation::rules_based::Attributes;
/// let attributes = [
///     ("age".into(), 30.0.into()),
///     ("is_premium_member".into(), true.into()),
///     ("username".into(), "john_doe".into()),
/// ].into_iter().collect::<Attributes>();
/// ```
pub type Attributes = HashMap<Str, AttributeValue>;

/// Attribute of a subject or action.
///
/// Stores attribute value (string, number, boolean) along with attribute kind (numeric or
/// categorical). Storing kind is helpful to make `Attributes` ↔ `ContextAttributes` conversion
/// isomorphic.
///
/// Note that attribute kind is stripped during serialization, so Attribute → JSON → Attribute
/// conversion is lossy.
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Serialize, Deserialize)]
#[from(NumericAttribute, CategoricalAttribute, f64, bool, Str, String, &str, Arc<str>, Arc<String>, Cow<'_, str>)]
pub struct AttributeValue(AttributeValueImpl);
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Deserialize)]
#[serde(untagged)]
enum AttributeValueImpl {
    #[from(NumericAttribute, f64)]
    Numeric(NumericAttribute),
    #[from(CategoricalAttribute, Str, bool, String, &str, Arc<str>, Arc<String>, Cow<'_, str>)]
    Categorical(CategoricalAttribute),
    #[from(ignore)]
    Null,
}

impl serde::Serialize for AttributeValueImpl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AttributeValueImpl::Numeric(numeric_attribute) => {
                numeric_attribute.serialize(serializer)
            }
            AttributeValueImpl::Categorical(categorical_attribute) => {
                categorical_attribute.serialize(serializer)
            }
            AttributeValueImpl::Null => serializer.serialize_none(),
        }
    }
}

impl AttributeValue {
    /// Create a numeric attribute.
    #[inline]
    pub fn numeric(value: impl Into<NumericAttribute>) -> AttributeValue {
        AttributeValue(AttributeValueImpl::Numeric(value.into()))
    }

    /// Create a categorical attribute.
    #[inline]
    pub fn categorical(value: impl Into<CategoricalAttribute>) -> AttributeValue {
        AttributeValue(AttributeValueImpl::Categorical(value.into()))
    }

    #[inline]
    pub const fn null() -> AttributeValue {
        AttributeValue(AttributeValueImpl::Null)
    }

    pub(crate) fn is_null(&self) -> bool {
        self == &AttributeValue(AttributeValueImpl::Null)
    }

    /// Try coercing attribute to a number.
    ///
    /// Number attributes are returned as is. For string attributes, we try to parse them into a
    /// number.
    pub(crate) fn coerce_to_number(&self) -> Option<f64> {
        match self.as_attribute_value()? {
            AttributeValueRef::Number(v) => Some(v),
            AttributeValueRef::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try coercing attribute to a string.
    ///
    /// String attributes are returned as is. Number and boolean attributes are converted to string.
    pub(crate) fn coerce_to_string(&self) -> Option<Cow<str>> {
        match self.as_attribute_value()? {
            AttributeValueRef::String(s) => Some(Cow::Borrowed(s)),
            AttributeValueRef::Number(v) => Some(Cow::Owned(v.to_string())),
            AttributeValueRef::Boolean(v) => Some(Cow::Borrowed(if v { "true" } else { "false" })),
        }
    }

    pub(crate) fn as_str(&self) -> Option<&Str> {
        match self {
            AttributeValue(AttributeValueImpl::Categorical(CategoricalAttribute(
                CategoricalAttributeImpl::String(s),
            ))) => Some(s),
            _ => None,
        }
    }

    fn as_attribute_value<'a>(&'a self) -> Option<AttributeValueRef<'a>> {
        self.into()
    }
}

/// Numeric attributes are quantitative (e.g., real numbers) and define a scale.
///
/// Not all numbers in programming are numeric attributes. If a number is used to represent an
/// enumeration or on/off values, it is a [categorical attribute](CategoricalAttribute).
#[derive(
    Debug,
    Clone,
    PartialEq,
    PartialOrd,
    derive_more::From,
    derive_more::Into,
    Serialize,
    Deserialize,
)]
pub struct NumericAttribute(f64);

/// Categorical attributes are attributes that have a finite set of values that are not directly
/// comparable (i.e., enumeration).
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Serialize, Deserialize)]
#[from(Str, f64, bool, String, &str, Arc<str>, Arc<String>, Cow<'_, str>)]
pub struct CategoricalAttribute(CategoricalAttributeImpl);
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Serialize, Deserialize)]
#[serde(untagged)]
enum CategoricalAttributeImpl {
    #[from(forward)]
    String(Str),
    #[from]
    Number(f64),
    #[from]
    Boolean(bool),
}

/// Enum representing values of an attribute.
///
/// It's a intermediate non-owning representation.
#[derive(Debug, Clone, Copy)]
enum AttributeValueRef<'a> {
    /// A string value.
    String(&'a Str),
    /// A numerical value.
    Number(f64),
    /// A boolean value.
    Boolean(bool),
}

impl<'a> From<&'a AttributeValue> for Option<AttributeValueRef<'a>> {
    fn from(value: &'a AttributeValue) -> Self {
        match value {
            AttributeValue(AttributeValueImpl::Numeric(numeric)) => {
                Some(AttributeValueRef::from(numeric))
            }
            AttributeValue(AttributeValueImpl::Categorical(categorical)) => {
                Some(AttributeValueRef::from(categorical))
            }
            AttributeValue(AttributeValueImpl::Null) => None,
        }
    }
}

impl From<&NumericAttribute> for AttributeValueRef<'_> {
    #[inline]
    fn from(value: &NumericAttribute) -> Self {
        AttributeValueRef::Number(value.0)
    }
}

impl<'a> From<&'a CategoricalAttribute> for AttributeValueRef<'a> {
    fn from(value: &'a CategoricalAttribute) -> Self {
        match value {
            CategoricalAttribute(CategoricalAttributeImpl::String(v)) => {
                AttributeValueRef::String(v)
            }
            CategoricalAttribute(CategoricalAttributeImpl::Number(v)) => {
                AttributeValueRef::Number(*v)
            }
            CategoricalAttribute(CategoricalAttributeImpl::Boolean(v)) => {
                AttributeValueRef::Boolean(*v)
            }
        }
    }
}
