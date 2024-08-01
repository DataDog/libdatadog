// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use std::ops::Not;

pub fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
    let s = s.try_to_utf8()?.to_string();
    Ok(s.is_empty().not().then_some(s))
}

/// A generic result type for when a crashtracking operation may fail,
/// but there's nothing to return in the case of success.
#[repr(C)]
pub enum Result {
    Ok(
        /// Do not use the value of Ok. This value only exists to overcome
        /// Rust -> C code generation.
        bool,
    ),
    Err(Error),
}

impl From<anyhow::Result<()>> for Result {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(_) => Self::Ok(true),
            Err(err) => Self::Err(err.into()),
        }
    }
}
