// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Environment-variable capability trait and error types.
//!
//! Sync: env access is a single map lookup on both native (`std::env`) and
//! wasm (`process.env`).
//!
//! `set` and `unset` are intentionally absent from this trait. libdatadog is
//! embedded in many kinds of runtime, where mutating the process environment
//! very much unsafe. Exposing mutation here would make it trivially easy for
//! callers to corrupt the environment of a multi-threaded host process.

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("The value of the environment variable `{0}` is not valid UTF-8")]
    NotUnicode(String),
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
}
