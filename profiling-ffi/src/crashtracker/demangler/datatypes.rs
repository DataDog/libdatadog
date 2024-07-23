// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::{Error, StringWrapper};

#[repr(C)]
pub enum DemangleOptions {
    Complete,
    NameOnly,
}

#[repr(C)]
pub enum StringWrapperResult {
    Ok(StringWrapper),
    #[allow(dead_code)]
    Err(Error),
}

// Useful for testing
impl StringWrapperResult {
    pub fn unwrap(self) -> StringWrapper {
        match self {
            StringWrapperResult::Ok(s) => s,
            StringWrapperResult::Err(e) => panic!("{e}"),
        }
    }
}

impl From<anyhow::Result<String>> for StringWrapperResult {
    fn from(value: anyhow::Result<String>) -> Self {
        match value {
            Ok(x) => Self::Ok(x.into()),
            Err(err) => Self::Err(err.into()),
        }
    }
}

impl From<String> for StringWrapperResult {
    fn from(value: String) -> Self {
        Self::Ok(value.into())
    }
}
