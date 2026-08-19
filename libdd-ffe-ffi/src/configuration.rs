// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::ensure;
use function_name::named;

use libdd_common_ffi::{wrap_with_ffi_result, Result};
use libdd_ffe::rules_based::{Configuration, UniversalFlagConfig};

use crate::{BorrowedStr, Handle};

/// Creates a new Configuration from JSON bytes.
///
/// # Ownership
///
/// The caller must call `ddog_ffe_configuration_drop` to release resources allocated for
/// configuration.
///
/// # Safety
///
/// - `json_bytes` must point to valid memory.
#[no_mangle]
#[named]
#[must_use]
pub unsafe extern "C" fn ddog_ffe_configuration_new(
    json_bytes: BorrowedStr,
) -> Result<Handle<Configuration>> {
    wrap_with_ffi_result!({
        ensure!(!json_bytes.ptr.is_null(), "json_str must not be NULL");

        // SAFETY: the caller must ensure that it's a valid pointer, we also checked for null
        let json_bytes = unsafe { json_bytes.as_bytes() }.to_vec();

        let configuration =
            Configuration::from_server_response(UniversalFlagConfig::from_json(json_bytes)?);

        Ok(Handle::from(configuration))
    })
}

/// Frees a Configuration.
///
/// # Safety
///
/// `config` must be a valid Configuration handle created by `ddog_ffe_configuration_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_configuration_drop(config: *mut Handle<Configuration>) {
    // SAFETY: the caller must ensure that config is a valid handle
    unsafe { Handle::free(config) };
}

/// Get the config-level `observeFullEvaluationData` flag.
///
/// Opt-in boolean for emitting full flag evaluation data from SDKs to Datadog. Flag evaluation data
/// may contain PII; defaults to `false` for privacy.
///
/// # Safety
///
/// `config` must be a valid `Configuration` handle created by `ddog_ffe_configuration_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_configuration_get_observe_full_evaluation_data(
    config: Handle<Configuration>,
) -> bool {
    // SAFETY: the caller must ensure that config is a valid handle.
    unsafe { config.as_ref() }.observe_full_evaluation_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a configuration handle through the real FFI entry point, reads the flag back through
    /// the FFI getter, then releases the handle.
    fn observe_full_evaluation_data_via_ffi(json: &str) -> bool {
        // `json` outlives this call, so the borrowed pointer stays valid.
        let borrowed = BorrowedStr {
            ptr: json.as_ptr(),
            len: json.len(),
        };

        // SAFETY: `borrowed` points to valid memory.
        let result = unsafe { ddog_ffe_configuration_new(borrowed) };
        let mut handle = match result {
            Result::Ok(handle) => handle,
            Result::Err(err) => panic!("configuration must parse: {err:?}"),
        };

        // The getter takes the handle by value, which C callers do without giving up ownership.
        // `Handle` is `#[repr(transparent)]` and has no `Drop`, so a bitwise copy is sound here and
        // lets us still drop the original exactly once below.
        // SAFETY: `handle` was just created by `ddog_ffe_configuration_new`.
        let handle_copy = unsafe { std::ptr::read(&handle) };

        // SAFETY: `handle_copy` refers to the live configuration created above.
        let observe =
            unsafe { ddog_ffe_configuration_get_observe_full_evaluation_data(handle_copy) };

        // SAFETY: `handle` is still valid and is freed exactly once.
        unsafe { ddog_ffe_configuration_drop(&mut handle) };

        observe
    }

    fn config_json(observe_field: &str) -> String {
        format!(
            r#"
              {{
                "createdAt": "2024-07-18T00:00:00Z",
                "environment": {{ "name": "test" }}
                {observe_field},
                "flags": {{}}
              }}
            "#
        )
    }

    #[test]
    fn ffi_getter_returns_true_when_field_is_true() {
        let json = config_json(r#", "observeFullEvaluationData": true"#);
        assert!(observe_full_evaluation_data_via_ffi(&json));
    }

    #[test]
    fn ffi_getter_returns_false_when_field_is_false() {
        let json = config_json(r#", "observeFullEvaluationData": false"#);
        assert!(!observe_full_evaluation_data_via_ffi(&json));
    }

    #[test]
    fn ffi_getter_returns_false_when_field_is_absent() {
        let json = config_json("");
        assert!(!observe_full_evaluation_data_via_ffi(&json));
    }

    /// A malformed value must not fail configuration creation through the FFI boundary.
    #[test]
    fn ffi_getter_returns_false_when_field_is_malformed() {
        let json = config_json(r#", "observeFullEvaluationData": null"#);
        assert!(!observe_full_evaluation_data_via_ffi(&json));
    }
}
