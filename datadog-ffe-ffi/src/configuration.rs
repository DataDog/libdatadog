// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::ensure;
use function_name::named;

use datadog_ffe::rules_based::{Configuration, UniversalFlagConfig};
use ddcommon_ffi::{wrap_with_ffi_result, Result};

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
