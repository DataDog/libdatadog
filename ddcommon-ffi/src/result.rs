// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Error;

/// A generic result type for when an operation may fail,
/// but there's nothing to return in the case of success.
#[repr(C)]
pub enum VoidResult {
    Ok(
        /// Do not use the value of Ok. This value only exists to overcome
        /// Rust -> C code generation.
        bool,
    ),
    Err(Error),
}

impl From<anyhow::Result<()>> for VoidResult {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(_) => Self::Ok(true),
            Err(err) => Self::Err(err.into()),
        }
    }
}

/// A generic result type for when an operation may fail,
/// or may return <T> in case of success.
#[repr(C)]
pub enum Result<T> {
    Ok(T),
    Err(Error),
}

impl<T> From<anyhow::Result<T>> for Result<T> {
    fn from(value: anyhow::Result<T>) -> Self {
        match value {
            Ok(v) => Self::Ok(v),
            Err(err) => Self::Err(err.into()),
        }
    }
}
