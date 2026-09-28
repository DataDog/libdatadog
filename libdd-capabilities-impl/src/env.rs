// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native environment-variable capability backed by `std::env`.

use std::env::VarError;

use libdd_capabilities::env::{EnvCapability, EnvError};

#[derive(Clone, Debug)]
pub struct NativeEnvCapability;

impl EnvCapability for NativeEnvCapability {
    fn new() -> Self {
        Self
    }

    fn get(&self, name: &str) -> Result<Option<String>, EnvError> {
        match std::env::var(name) {
            Ok(v) => Ok(Some(v)),
            Err(VarError::NotPresent) => Ok(None),
            Err(VarError::NotUnicode(_)) => Err(EnvError::NotUnicode(name.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_absent_returns_none() {
        let cap = NativeEnvCapability;
        // A name unlikely to be set in any environment.
        assert_eq!(cap.get("LIBDD_CAP_TEST_ABSENT_XYZZY").unwrap(), None);
    }
}
