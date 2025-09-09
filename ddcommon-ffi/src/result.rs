// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Error;

/// A generic result type for when an operation may fail,
/// but there's nothing to return in the case of success.
#[repr(C)]
pub enum VoidResult {
    Ok,
    Err(Error),
}

impl VoidResult {
    pub fn unwrap(self) {
        assert!(matches!(self, Self::Ok));
    }

    pub fn unwrap_err(self) -> Error {
        match self {
            #[allow(clippy::panic)]
            Self::Ok => panic!("Expected error, got value"),
            Self::Err(err) => err,
        }
    }
}

impl From<anyhow::Result<()>> for VoidResult {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(_) => Self::Ok,
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

impl<T> Result<T> {
    pub fn unwrap(self) -> T {
        match self {
            Self::Ok(v) => v,
            #[allow(clippy::panic)]
            Self::Err(err) => panic!("{err}"),
        }
    }

    pub fn unwrap_err(self) -> Error {
        match self {
            #[allow(clippy::panic)]
            Self::Ok(_) => panic!("Expected error, got value"),
            Self::Err(err) => err,
        }
    }
}

impl<T> From<anyhow::Result<T>> for Result<T> {
    fn from(value: anyhow::Result<T>) -> Self {
        match value {
            Ok(v) => Self::Ok(v),
            Err(err) => Self::Err(err.into()),
        }
    }
}
