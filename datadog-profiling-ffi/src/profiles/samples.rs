// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_alloc::Box;
use datadog_profiling::profiles::{
    self, ProfileError, ProfileVoidResult, SampleManager, SliceId,
};
use datadog_profiling_protobuf::ValueType;
use ddcommon_ffi::Slice;
use std::ptr;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Sample<'a> {
    stack_trace_id: SliceId,
    values: Slice<'a, i64>,
    /// Don't use a timestamp label, use the `timestamp` field for that.
    labels: SliceId,
    /// Use 0 for no timestamp, we can do `Option<NonZeroI64>` one day. This
    /// timestamp is nanoseconds since the Unix epoch.
    timestamp: i64,
}

impl<'a> TryFrom<Sample<'a>> for profiles::Sample<'a> {
    type Error = ProfileError;

    fn try_from(value: Sample<'a>) -> Result<Self, Self::Error> {
        Ok(profiles::Sample {
            stack_trace_id: value.stack_trace_id,
            values: value
                .values
                .try_as_slice()
                .ok_or(ProfileError::InvalidInput)?,
            labels: value.labels,
            timestamp: value.timestamp,
        })
    }
}

#[repr(C)]
pub enum SampleManagerNewResult {
    Ok(*mut SampleManager),
    Err(ProfileError),
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_SampleManager_new(
    sample_types: Slice<ValueType>,
) -> SampleManagerNewResult {
    let Some(sample_types) = sample_types.try_as_slice() else {
        return SampleManagerNewResult::Err(ProfileError::InvalidInput);
    };

    let sample_manager =
        match SampleManager::new(sample_types.iter().copied().map(Ok)) {
            Ok(ok) => ok,
            Err(err) => return SampleManagerNewResult::Err(err),
        };

    let Ok(boxed) = Box::try_new(sample_manager) else {
        return SampleManagerNewResult::Err(ProfileError::OutOfMemory);
    };
    SampleManagerNewResult::Ok(Box::into_raw(boxed))
}

/// # Safety
///
/// The `sample_manager` must be a valid pointer to a `SampleManager`.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_SampleManager_add_sample(
    sample_manager: Option<&mut SampleManager>,
    sample: Sample,
) -> ProfileVoidResult {
    let Some(sample_manager) = sample_manager else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    let sample = match sample.try_into() {
        Ok(sample) => sample,
        Err(err) => return ProfileVoidResult::Err(err),
    };

    ProfileVoidResult::from(sample_manager.add_sample(sample))
}

/// # Safety
///
/// The `m` must be a valid pointer to a pointer to a `SampleManager`.
/// `*m` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleManager_drop(
    m: *mut *mut SampleManager,
) {
    if let Some(ptr) = m.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}
