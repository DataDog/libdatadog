// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Some string type helpers.
//!
//! Moved into a separate module, so we could experiment with different representations.

use std::{borrow::Cow, string::FromUtf8Error, sync::Arc};

use faststr::FastStr;

use serde::{Deserialize, Serialize};

/// `Str` is a string optimized for cheap cloning. The implementation is hidden, so we can update it
/// if we find faster implementation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Str(FastStr);

impl Str {
    pub fn new<S: AsRef<str>>(s: S) -> Str {
        Str(FastStr::new(s))
    }

    pub fn from_static_str(s: &'static str) -> Str {
        Str(FastStr::from_static_str(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::fmt::Display for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! impl_from_faststr {
    ($ty:ty) => {
        impl From<$ty> for Str {
            fn from(value: $ty) -> Str {
                Str(value.into())
            }
        }
    };
}

impl_from_faststr!(Arc<str>);
impl_from_faststr!(Arc<String>);
impl_from_faststr!(String);

impl<'a> From<&'a str> for Str {
    fn from(value: &'a str) -> Str {
        Str(FastStr::new(value))
    }
}

impl<'a> From<Cow<'a, str>> for Str {
    fn from(value: Cow<'a, str>) -> Str {
        match value {
            Cow::Borrowed(s) => s.into(),
            Cow::Owned(s) => s.into(),
        }
    }
}

impl TryFrom<Vec<u8>> for Str {
    type Error = FromUtf8Error;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        String::from_utf8(value).map(Into::into)
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<[u8]> for Str {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl std::ops::Deref for Str {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl std::borrow::Borrow<str> for Str {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl log::kv::ToValue for Str {
    fn to_value(&self) -> log::kv::Value<'_> {
        log::kv::Value::from_display(self)
    }
}
