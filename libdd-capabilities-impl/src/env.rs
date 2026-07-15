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

    fn set(&self, name: &str, value: &str) -> Result<(), EnvError> {
        std::env::set_var(name, value);
        Ok(())
    }

    fn unset(&self, name: &str) -> Result<(), EnvError> {
        std::env::remove_var(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-mutating tests; std::env is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn get_absent_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        let cap = NativeEnvCapability;
        // A name unlikely to be set in any environment.
        assert_eq!(cap.get("LIBDD_CAP_TEST_ABSENT_XYZZY").unwrap(), None);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let _g = ENV_LOCK.lock().unwrap();
        let cap = NativeEnvCapability;
        cap.set("LIBDD_CAP_TEST_SET", "value").unwrap();
        assert_eq!(
            cap.get("LIBDD_CAP_TEST_SET").unwrap(),
            Some("value".to_owned())
        );
        cap.unset("LIBDD_CAP_TEST_SET").unwrap();
    }

    #[test]
    fn unset_then_get_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        let cap = NativeEnvCapability;
        cap.set("LIBDD_CAP_TEST_UNSET", "value").unwrap();
        cap.unset("LIBDD_CAP_TEST_UNSET").unwrap();
        assert_eq!(cap.get("LIBDD_CAP_TEST_UNSET").unwrap(), None);
    }
}
