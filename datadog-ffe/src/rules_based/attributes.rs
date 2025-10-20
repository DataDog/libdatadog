use std::{borrow::Cow, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::rules_based::Str;

/// Attribute for evaluation context. See `From` implementations for initialization.
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Serialize, Deserialize)]
#[from(f64, bool, Str, String, &str, Arc<str>, Arc<String>, Cow<'_, str>)]
pub struct Attribute(AttributeValueImpl);
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize, derive_more::From)]
#[serde(untagged)]
enum AttributeValueImpl {
    #[from]
    Number(f64),
    #[from(forward)]
    String(Str),
    #[from]
    Boolean(bool),
    #[from(ignore)]
    Null,
}

impl Attribute {
    pub(crate) fn is_null(&self) -> bool {
        self == &Attribute(AttributeValueImpl::Null)
    }

    /// Try coercing attribute to a number.
    ///
    /// Number attributes are returned as is. For string attributes, we try to parse them into a
    /// number.
    pub(crate) fn coerce_to_number(&self) -> Option<f64> {
        match &self.0 {
            AttributeValueImpl::Number(v) => Some(*v),
            AttributeValueImpl::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try coercing attribute to a string.
    ///
    /// String attributes are returned as is. Number and boolean attributes are converted to string.
    pub(crate) fn coerce_to_string(&self) -> Option<Cow<'_, str>> {
        match &self.0 {
            AttributeValueImpl::String(s) => Some(Cow::Borrowed(s)),
            AttributeValueImpl::Number(v) => Some(Cow::Owned(v.to_string())),
            AttributeValueImpl::Boolean(v) => {
                Some(Cow::Borrowed(if *v { "true" } else { "false" }))
            }
            AttributeValueImpl::Null => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&Str> {
        match self {
            Attribute(AttributeValueImpl::String(s)) => Some(s),
            _ => None,
        }
    }
}
