// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native environment-variable capability backed by `std::env`.

use std::env::VarError;

use libdd_capabilities::env::{validate_name, validate_value, EnvCapability, EnvError};

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

    unsafe fn set(&self, name: &str, value: &str) -> Result<(), EnvError> {
        validate_name(name)?;
        validate_value(value)?;
        // SAFETY: Caller upholds the single-threaded-env precondition
        // documented on `EnvCapability::set`. Inputs have been validated so
        // `std::env::set_var` will not panic.
        unsafe {
            std::env::set_var(name, value);
        }
        Ok(())
    }

    unsafe fn unset(&self, name: &str) -> Result<(), EnvError> {
        validate_name(name)?;
        // SAFETY: Caller upholds the single-threaded-env precondition
        // documented on `EnvCapability::unset`. Name has been validated so
        // `std::env::remove_var` will not panic.
        unsafe {
            std::env::remove_var(name);
        }
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
        unsafe { cap.set("LIBDD_CAP_TEST_SET", "value").unwrap() };
        assert_eq!(
            cap.get("LIBDD_CAP_TEST_SET").unwrap(),
            Some("value".to_owned())
        );
        unsafe { cap.unset("LIBDD_CAP_TEST_SET").unwrap() };
    }

    #[test]
    fn unset_then_get_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        let cap = NativeEnvCapability;
        unsafe {
            cap.set("LIBDD_CAP_TEST_UNSET", "value").unwrap();
            cap.unset("LIBDD_CAP_TEST_UNSET").unwrap();
        }
        assert_eq!(cap.get("LIBDD_CAP_TEST_UNSET").unwrap(), None);
    }

    #[test]
    fn set_rejects_invalid_name() {
        let cap = NativeEnvCapability;
        // SAFETY: validation fails before any env mutation happens.
        unsafe {
            assert!(matches!(cap.set("", "v"), Err(EnvError::Invalid(_))));
            assert!(matches!(cap.set("A=B", "v"), Err(EnvError::Invalid(_))));
            assert!(matches!(cap.set("A\0B", "v"), Err(EnvError::Invalid(_))));
        }
    }

    #[test]
    fn set_rejects_invalid_value() {
        let cap = NativeEnvCapability;
        // SAFETY: validation fails before any env mutation happens.
        unsafe {
            assert!(matches!(
                cap.set("LIBDD_CAP_TEST_INVALID_VALUE", "a\0b"),
                Err(EnvError::Invalid(_))
            ));
        }
    }
}
