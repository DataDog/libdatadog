// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use datadog_ffe::rules_based::{Configuration, UniversalFlagConfig};
use ddcommon_ffi::{Handle, ToInner};

/// Creates a new Configuration from JSON bytes
/// 
/// # Safety
/// `json_str` must be a valid null-terminated C string containing valid JSON
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_configuration_new(
    json_str: *const c_char,
) -> Handle<Configuration> {
    if json_str.is_null() {
        return Handle::empty();
    }

    let json_cstr = match CStr::from_ptr(json_str).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let json_bytes = json_cstr.as_bytes().to_vec();

    match UniversalFlagConfig::from_json(json_bytes) {
        Ok(universal_config) => {
            let config = Configuration::from_server_response(universal_config);
            Handle::from(config)
        }
        Err(_) => Handle::empty(),
    }
}

/// Frees a Configuration
/// 
/// # Safety
/// `config` must be a valid Configuration handle created by `ddog_ffe_configuration_new`
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_configuration_drop(mut config: *mut Handle<Configuration>) {
    drop(config.take());
}
