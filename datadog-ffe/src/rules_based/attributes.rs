// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{borrow::Cow, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::rules_based::Str;

/// Attribute for evaluation context. See `From` implementations for initialization.
#[derive(Debug, Clone, PartialEq, PartialOrd, derive_more::From, Serialize, Deserialize)]
#[from(f64, bool, Str, String, &str, Arc<str>, Arc<String>, Cow<'_, str>)]
pub struct Attribute(AttributeImpl);
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize, derive_more::From)]
#[serde(untagged)]
enum AttributeImpl {
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
        self == &Attribute(AttributeImpl::Null)
    }

    /// Try coercing attribute to a number.
    ///
    /// Number attributes are returned as is. For string attributes, we try to parse them into a
    /// number.
    pub(crate) fn coerce_to_number(&self) -> Option<f64> {
        match &self.0 {
            AttributeImpl::Number(v) => Some(*v),
            AttributeImpl::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try coercing attribute to a string.
    ///
    /// String attributes are returned as is. Number and boolean attributes are converted to string.
    pub(crate) fn coerce_to_string(&self) -> Option<Cow<'_, str>> {
        match &self.0 {
            AttributeImpl::String(s) => Some(Cow::Borrowed(s)),
            AttributeImpl::Number(v) => Some(Cow::Owned(v.to_string())),
            AttributeImpl::Boolean(v) => Some(Cow::Borrowed(if *v { "true" } else { "false" })),
            AttributeImpl::Null => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&Str> {
        match self {
            Attribute(AttributeImpl::String(s)) => Some(s),
            _ => None,
        }
    }
}

#[cfg(feature = "pyo3")]
mod pyo3_impl {
    use super::*;

    use pyo3::{
        exceptions::PyTypeError,
        prelude::*,
        types::{PyBool, PyFloat, PyInt, PyString},
    };

    /// Convert Python value to Attribute.
    ///
    /// The following types are currently supported:
    /// - `str`
    /// - `int`
    /// - `float`
    /// - `bool`
    /// - `NoneType`
    ///
    /// Note that nesting is not currently supported and will throw an error.
    impl<'py> FromPyObject<'py> for Attribute {
        #[inline]
        fn extract_bound(value: &Bound<'py, PyAny>) -> PyResult<Self> {
            if let Ok(s) = value.downcast::<PyString>() {
                return Ok(Attribute(AttributeImpl::String(s.to_cow()?.into())));
            }
            // In Python, Bool inherits from Int, so it must be checked first here.
            if let Ok(s) = value.downcast::<PyBool>() {
                return Ok(Attribute(AttributeImpl::Boolean(s.is_true())));
            }
            if let Ok(s) = value.downcast::<PyFloat>() {
                return Ok(Attribute(AttributeImpl::Number(s.value())));
            }
            if let Ok(s) = value.downcast::<PyInt>() {
                return Ok(Attribute(AttributeImpl::Number(s.extract::<f64>()?)));
            }
            if value.is_none() {
                return Ok(Attribute(AttributeImpl::Null));
            }
            Err(PyTypeError::new_err("invalid type for attribute"))
        }
    }
}
