// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Minimal stand-in for `core::io::{Error, ErrorKind, Result}`.
//!
//! These types are moving into `core` behind the unstable `core_io` feature
//! (<https://github.com/rust-lang/rust/issues/154046>). This module mirrors the subset of that API
//! available in `core`, so once it is stable and within our MSRV it can be replaced by
//! `pub use core::io::{Error, ErrorKind, Result};` without changing [`ConfigRead`] implementations.
//!
//! [`ConfigRead`]: crate::ConfigRead

use alloc::string::String;
use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

/// Subset of `core::io::ErrorKind` relevant to reading configuration files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    InvalidData,
    FileTooLarge,
    Other,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorKind::NotFound => "entity not found",
            ErrorKind::PermissionDenied => "permission denied",
            ErrorKind::InvalidData => "invalid data",
            ErrorKind::FileTooLarge => "file too large",
            ErrorKind::Other => "other error",
        })
    }
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    /// The original message, when converted from a `std::io::Error`.
    message: Option<String>,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self {
            kind,
            message: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.message {
            Some(message) => f.write_str(message),
            None => self.kind.fmt(f),
        }
    }
}

impl core::error::Error for Error {}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        let kind = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidData => ErrorKind::InvalidData,
            std::io::ErrorKind::FileTooLarge => ErrorKind::FileTooLarge,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: Some(err.to_string()),
        }
    }
}
