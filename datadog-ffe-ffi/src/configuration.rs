// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use anyhow::ensure;
use function_name::named;

use datadog_ffe::rules_based::{Configuration, UniversalFlagConfig};
use ddcommon_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};

/// Creates a new Configuration from JSON bytes
///
/// # Safety
/// `json_str` must be a valid C string.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_ffe_configuration_new(
    json_str: *const c_char,
) -> Result<Handle<Configuration>> {
    wrap_with_ffi_result!({
        ensure!(!json_str.is_null(), "json_str must not be NULL");

        let json_bytes = CStr::from_ptr(json_str).to_bytes().to_vec();

        let configuration =
            Configuration::from_server_response(UniversalFlagConfig::from_json(json_bytes)?);

        Ok(Handle::from(configuration))
    })
}

/// Frees a Configuration
///
/// # Safety
/// `config` must be a valid Configuration handle created by `ddog_ffe_configuration_new`
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_configuration_drop(mut config: *mut Handle<Configuration>) {
    drop(config.take());
}
