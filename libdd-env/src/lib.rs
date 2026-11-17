// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! Shared conventions for reading libdatadog configuration from environment
//! variables.
//!
//! Each function performs a one-shot read and returns [`None`] when the
//! variable is unset, is not valid Unicode, or fails to parse. A variable that
//! is set but holds an unexpected value is *not* [`None`]: [`env_bool`]
//! parses `"yes"` as `Some(false)`, keeping "unset" distinguishable from
//! "explicitly set". Concurrent `setenv` from host code can race with these
//! reads.

use core::{str::FromStr, time::Duration};

/// Reads the variable as a number of seconds (`f32`).
///
/// # Panics
///
/// Panics if the parsed value is negative or not finite, like
/// [`Duration::from_secs_f32`].
pub fn env_duration(name: &str) -> Option<Duration> {
    Some(Duration::from_secs_f32(
        std::env::var(name).ok()?.parse::<f32>().ok()?,
    ))
}

/// Reads the variable and parses it with [`FromStr`].
pub fn env_from_str<T: FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.parse::<T>().ok()
}

/// Reads the variable as a libdatadog boolean flag.
///
/// The truthy values are `"true"` and `"1"`, matched case-insensitively
/// (`"TRUE"` and `"True"` parse as true); any other set value parses as
/// `false`.
pub fn env_bool(name: &str) -> Option<bool> {
    let var = std::env::var(name).ok()?;
    Some(var == "1" || var.eq_ignore_ascii_case("true"))
}

/// Reads the variable, [`None`] when unset or empty.
pub fn env_str_not_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests mutate a shared environment variable, so they must not run
    // concurrently.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    const TEST_VAR: &str = "LIBDD_ENV_TEST_VALUE";

    fn set_var(value: &str) {
        unsafe { std::env::set_var(TEST_VAR, value) };
    }

    fn remove_var() {
        unsafe { std::env::remove_var(TEST_VAR) };
    }

    #[test]
    fn env_bool_matches_true_case_insensitively() {
        let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        remove_var();
        assert_eq!(env_bool(TEST_VAR), None);

        set_var("true");
        assert_eq!(env_bool(TEST_VAR), Some(true));
        set_var("TRUE");
        assert_eq!(env_bool(TEST_VAR), Some(true));
        set_var("True");
        assert_eq!(env_bool(TEST_VAR), Some(true));
        set_var("1");
        assert_eq!(env_bool(TEST_VAR), Some(true));
        set_var("yes");
        assert_eq!(env_bool(TEST_VAR), Some(false));
        set_var("");
        assert_eq!(env_bool(TEST_VAR), Some(false));
    }

    #[test]
    fn env_from_str_parses_with_t() {
        let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        remove_var();
        assert_eq!(env_from_str::<u16>(TEST_VAR), None);

        set_var("8126");
        assert_eq!(env_from_str::<u16>(TEST_VAR), Some(8126));
        set_var("-1");
        assert_eq!(env_from_str::<u16>(TEST_VAR), None);
        set_var("not-a-number");
        assert_eq!(env_from_str::<u16>(TEST_VAR), None);
    }

    #[test]
    fn env_duration_parses_seconds() {
        let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        remove_var();
        assert_eq!(env_duration(TEST_VAR), None);

        set_var("1.5");
        assert_eq!(env_duration(TEST_VAR), Some(Duration::from_secs_f32(1.5)));
        set_var("not-a-number");
        assert_eq!(env_duration(TEST_VAR), None);
    }

    #[test]
    fn env_str_not_empty_ignores_empty_values() {
        let _lock = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        remove_var();
        assert_eq!(env_str_not_empty(TEST_VAR), None);

        set_var("value");
        assert_eq!(env_str_not_empty(TEST_VAR), Some("value".to_owned()));
        set_var("");
        assert_eq!(env_str_not_empty(TEST_VAR), None);
    }
}
