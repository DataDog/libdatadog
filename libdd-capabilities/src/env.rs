// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Environment-variable capability trait and error types.
//!
//! Sync: env access is a single map lookup on both native (`std::env`) and
//! wasm (`process.env`); adding a future would only add ceremony.

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("Env var value is not valid UTF-8: {0}")]
    NotUnicode(String),
    #[error("Invalid env var name or value: {0}")]
    Invalid(&'static str),
    #[error("IO error: {0}")]
    Io(anyhow::Error),
}

pub trait EnvCapability: Clone + std::fmt::Debug {
    fn new() -> Self;

    /// Read an env var.
    ///
    /// `Ok(None)` means the variable is unset; `Err(NotUnicode)` means it is
    /// set but its value is not valid UTF-8. Callers that treat "missing" and
    /// "invalid" the same should collapse both branches explicitly.
    fn get(&self, name: &str) -> Result<Option<String>, EnvError>;

    /// Set an env var.
    ///
    /// # Safety
    /// No other thread may access the process environment concurrently.
    unsafe fn set(&self, name: &str, value: &str) -> Result<(), EnvError>;

    /// Unset an env var.
    ///
    /// # Safety
    /// No other thread may access the process environment concurrently.
    unsafe fn unset(&self, name: &str) -> Result<(), EnvError>;
}

/// Validate an env var name per the same rules `std::env::set_var` would
/// panic on: non-empty, no NUL byte, no `=` sign.
pub fn validate_name(name: &str) -> Result<(), EnvError> {
    if name.is_empty() {
        return Err(EnvError::Invalid("name is empty"));
    }
    if name.as_bytes().contains(&b'\0') {
        return Err(EnvError::Invalid("name contains NUL byte"));
    }
    if name.as_bytes().contains(&b'=') {
        return Err(EnvError::Invalid("name contains '=' character"));
    }
    Ok(())
}

/// Validate an env var value: no NUL byte (would panic in `std::env::set_var`).
pub fn validate_value(value: &str) -> Result<(), EnvError> {
    if value.as_bytes().contains(&b'\0') {
        return Err(EnvError::Invalid("value contains NUL byte"));
    }
    Ok(())
}
