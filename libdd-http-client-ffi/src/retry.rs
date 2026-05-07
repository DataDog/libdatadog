// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_http_client::RetryConfig`].
//!
//! Retry configs are owned by C as opaque `Box<RetryConfig>` pointers,
//! configured via builder-style setters, and ultimately attached to a
//! client builder via
//! [`crate::ddog_http_client_builder_set_retry`], which consumes the
//! config. If the caller decides to abandon a config before attaching it,
//! it must be released via [`ddog_retry_config_drop`].

use crate::error::DdogHttpClientError;
use libdd_http_client::RetryConfig;
use std::ptr::NonNull;
use std::time::Duration;

/// Allocate a new [`RetryConfig`] with default settings: 3 retries, 100ms
/// initial delay, exponential backoff with jitter.
///
/// Writes a `Box<RetryConfig>` into `*out_handle`. The caller owns the
/// handle and must eventually pass it to either
/// [`crate::ddog_http_client_builder_set_retry`] (which consumes it) or
/// [`ddog_retry_config_drop`].
///
/// # Safety
/// `out_handle` must be a valid, writable pointer to an uninitialised
/// `*mut ddog_RetryConfig`.
#[no_mangle]
pub unsafe extern "C" fn ddog_retry_config_new(out_handle: NonNull<Box<RetryConfig>>) {
    out_handle.as_ptr().write(Box::new(RetryConfig::new()));
}

/// Set the maximum number of retry attempts (not counting the initial
/// request).
///
/// # Safety
/// `cfg` must be `None` or a valid mutable reference to a config produced
/// by [`ddog_retry_config_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_retry_config_set_max_retries(
    cfg: Option<&mut RetryConfig>,
    max_retries: u32,
) -> Option<Box<DdogHttpClientError>> {
    let Some(c) = cfg else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "retry config is null",
        )));
    };
    let taken = std::mem::replace(c, RetryConfig::new());
    *c = taken.max_retries(max_retries);
    None
}

/// Set the initial delay before the first retry, in milliseconds.
/// Subsequent retries double this value (exponential backoff).
///
/// # Safety
/// `cfg` must be `None` or a valid mutable reference to a config produced
/// by [`ddog_retry_config_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_retry_config_set_initial_delay_millis(
    cfg: Option<&mut RetryConfig>,
    initial_delay_ms: u64,
) -> Option<Box<DdogHttpClientError>> {
    let Some(c) = cfg else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "retry config is null",
        )));
    };
    let taken = std::mem::replace(c, RetryConfig::new());
    *c = taken.initial_delay(Duration::from_millis(initial_delay_ms));
    None
}

/// Enable or disable jitter. When enabled, each delay is replaced with a
/// uniform random value between 0 and the calculated delay.
///
/// # Safety
/// `cfg` must be `None` or a valid mutable reference to a config produced
/// by [`ddog_retry_config_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_retry_config_set_jitter(
    cfg: Option<&mut RetryConfig>,
    jitter: bool,
) -> Option<Box<DdogHttpClientError>> {
    let Some(c) = cfg else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "retry config is null",
        )));
    };
    let taken = std::mem::replace(c, RetryConfig::new());
    *c = taken.with_jitter(jitter);
    None
}

/// Drop a retry config that was not attached to a builder.
///
/// # Safety
/// `cfg` must be `None` or a config produced by
/// [`ddog_retry_config_new`] and not yet consumed by
/// [`crate::ddog_http_client_builder_set_retry`].
#[no_mangle]
pub unsafe extern "C" fn ddog_retry_config_drop(cfg: Option<Box<RetryConfig>>) {
    drop(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn new_config_defaults() {
        unsafe {
            let mut cfg: MaybeUninit<Box<RetryConfig>> = MaybeUninit::uninit();
            ddog_retry_config_new(NonNull::new_unchecked(cfg.as_mut_ptr()));
            let cfg = cfg.assume_init();
            ddog_retry_config_drop(Some(cfg));
        }
    }

    #[test]
    fn setters_round_trip() {
        unsafe {
            let mut cfg: MaybeUninit<Box<RetryConfig>> = MaybeUninit::uninit();
            ddog_retry_config_new(NonNull::new_unchecked(cfg.as_mut_ptr()));
            let mut cfg = Some(cfg.assume_init());

            assert!(ddog_retry_config_set_max_retries(cfg.as_deref_mut(), 5).is_none());
            assert!(
                ddog_retry_config_set_initial_delay_millis(cfg.as_deref_mut(), 250).is_none()
            );
            assert!(ddog_retry_config_set_jitter(cfg.as_deref_mut(), false).is_none());

            ddog_retry_config_drop(cfg);
        }
    }

    #[test]
    fn null_cfg_returns_invalid_argument() {
        unsafe {
            let err = ddog_retry_config_set_max_retries(None, 3);
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
            let err = ddog_retry_config_set_initial_delay_millis(None, 100);
            assert!(err.is_some());
            let err = ddog_retry_config_set_jitter(None, true);
            assert!(err.is_some());
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_retry_config_drop(None) };
    }
}
