// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Access to environment-like key/value data.

use core::{convert::Infallible, fmt};

/// Interface for reading copied operating-system environment values.
pub trait OsEnv {
    /// Error returned by the source.
    type Error: fmt::Debug;
    /// Value returned by the source.
    type Value: AsRef<str>;

    /// Returns the copied value for `key`, or `None` when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns a source-specific error when the value cannot be read.
    fn get(&self, key: &str) -> Result<Option<Self::Value>, Self::Error>;
}

impl<T> OsEnv for &T
where
    T: OsEnv + ?Sized,
{
    type Error = T::Error;
    type Value = T::Value;

    fn get(&self, key: &str) -> Result<Option<Self::Value>, Self::Error> {
        T::get(self, key)
    }
}

/// A borrowed key/value environment backed by a slice.
#[derive(Clone, Copy, Debug)]
pub struct Environment<'a> {
    entries: &'a [(&'a str, &'a str)],
}

impl<'a> Environment<'a> {
    /// Creates an environment over `entries`.
    #[must_use]
    pub const fn new(entries: &'a [(&'a str, &'a str)]) -> Self {
        Self { entries }
    }

    /// Returns the environment entries.
    #[must_use]
    pub const fn entries(&self) -> &'a [(&'a str, &'a str)] {
        self.entries
    }
}

impl<'a> OsEnv for Environment<'a> {
    type Error = Infallible;
    type Value = &'a str;

    fn get(&self, key: &str) -> Result<Option<Self::Value>, Self::Error> {
        Ok(self
            .entries
            .iter()
            .find(|(entry, _)| *entry == key)
            .map(|(_, value)| *value))
    }
}

#[cfg(test)]
mod tests {
    use super::{Environment, OsEnv};

    const ENTRIES: &[(&str, &str)] = &[("agent.internal", "127.0.0.1")];

    #[test]
    fn returns_a_value() {
        let environment = Environment::new(ENTRIES);
        assert_eq!(environment.get("agent.internal"), Ok(Some("127.0.0.1")));
    }

    #[test]
    fn reports_a_missing_value() {
        let environment = Environment::new(ENTRIES);
        assert_eq!(environment.get("missing"), Ok(None));
    }
}
