// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Environment-variable capability trait and error types.
//!
//! Reads borrow UTF-8 values from stable storage, without allocating. Implementations that
//! read a mutable native or host environment must first capture storage they can safely borrow.
//!
//! `set` and `unset` are intentionally absent from this trait. libdatadog is
//! embedded in many kinds of runtime, where mutating the process environment
//! very much unsafe. Exposing mutation here would make it trivially easy for
//! callers to corrupt the environment of a multi-threaded host process.

/// Failure to read an environment variable as UTF-8, without allocating an error payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvError {
    /// The value is present but not valid UTF-8. The caller already knows the variable name.
    NotUnicode,
}

impl core::fmt::Display for EnvError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotUnicode => f.write_str("environment variable value is not valid UTF-8"),
        }
    }
}

impl core::error::Error for EnvError {}

pub trait EnvCapability: Clone + core::fmt::Debug {
    fn new() -> Self;

    /// Borrow an environment variable without allocating.
    ///
    /// `Ok(None)` means the variable is unset; `Err(NotUnicode)` means it is
    /// set but its value is not valid UTF-8. Callers that treat "missing" and
    /// "invalid" the same should collapse both branches explicitly.
    ///
    /// The returned value borrows from `self`, independently of `name`. Implementations
    /// keep its backing storage valid for that borrow. Callers needing ownership can
    /// explicitly copy the value.
    fn get(&self, name: &str) -> Result<Option<&str>, EnvError>;
}
