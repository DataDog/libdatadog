// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::datatypes::{profile_ptr_to_inner, Profile};
use function_name::named;
use libdd_common_ffi::{wrap_with_void_ffi_result, VoidResult};

/// Legacy profile interning FFI APIs were removed in this draft branch.
/// Use dictionary-backed flows (`ddog_prof_ProfilesDictionary_*` +
/// `ddog_prof_Profile_add2`) instead.
///
/// This module now only exposes sample synchronization helpers kept for
/// compatibility with existing exporter coordination logic.

/// This functions ends the current sample and allows the profiler exporter to continue, if it was
/// blocked.
/// It must have been paired with exactly one `sample_start`.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is probably thread-safe, but I haven't confirmed this.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_sample_end(profile: *mut Profile) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile_ptr_to_inner(profile)?.sample_end()?;
    })
}

/// This functions starts a sample and blocks the exporter from continuing.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is probably thread-safe, but I haven't confirmed this.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_sample_start(profile: *mut Profile) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile_ptr_to_inner(profile)?.sample_start()?;
    })
}
