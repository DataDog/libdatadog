// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_handle::ProfileHandle;
use crate::profiles::ensure_non_null_out_parameter;
use crate::ProfileStatus;
use datadog_profiling::profiles::datatypes::{Profile, ValueType};
use datadog_profiling::profiles::ProfileError;

/// Allocates a new `Profile` and writes a handle to `handle`.
///
/// # Safety
///
/// - `handle` must be non-null and valid for writes of `ProfileHandle<_>`.
/// - The written handle must be dropped via the matching drop function;
///   see [`ddog_prof_Profile_drop`] for more details.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_new(
    handle: *mut ProfileHandle<Profile>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let h = ProfileHandle::try_new(Profile::default())?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

/// Adds a sample type to a profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique
///   mutable reference for the duration of the call (no aliasing mutations).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_sample_type(
    mut handle: ProfileHandle<Profile>,
    vt: ValueType,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { handle.as_inner_mut()? };
        prof.add_sample_type(vt)
    }())
}

/// Sets the period and adds its `ValueType` to the profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique
///   mutable reference for the duration of the call (no aliasing mutations).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_period(
    mut handle: ProfileHandle<Profile>,
    period: i64,
    vt: ValueType,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { handle.as_inner_mut()? };
        prof.add_period(period, vt);
        Ok(())
    }())
}

/// Drops the contents of the profile handle, leaving an empty handle behind.
///
/// # Safety
///
/// Pointer must point to a valid profile handle if not null.
///
/// The underlying resource must only be dropped through a single handle, and
/// once the underlying profile has been dropped, all other handles are invalid
/// and should be discarded without dropping them.
///
/// However, this function is safe to call multiple times on the _same handle_.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_drop(
    handle: *mut ProfileHandle<Profile>,
) {
    if let Some(h) = handle.as_mut() {
        drop(h.take());
    }
}
