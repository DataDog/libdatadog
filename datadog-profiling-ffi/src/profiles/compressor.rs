// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::profiles::{Compressor, ProfileError};
use ddcommon_ffi as ffi;
use std::ptr;

#[must_use]
#[no_mangle]
pub extern "C" fn ddog_prof_Compressor_new(max_capacity: usize) -> Compressor {
    Compressor::with_max_capacity(max_capacity)
}

#[repr(C)]
pub enum CompressorFinishResult {
    Ok(ffi::Vec<u8>),
    Err(ProfileError),
}

impl From<CompressorFinishResult> for Result<Vec<u8>, ProfileError> {
    fn from(result: CompressorFinishResult) -> Self {
        match result {
            CompressorFinishResult::Ok(vec) => Ok(vec.into()),
            CompressorFinishResult::Err(err) => Err(err),
        }
    }
}

impl From<Result<Vec<u8>, ProfileError>> for CompressorFinishResult {
    fn from(result: Result<Vec<u8>, ProfileError>) -> Self {
        match result {
            Ok(ok) => CompressorFinishResult::Ok(ok.into()),
            Err(err) => CompressorFinishResult::Err(err),
        }
    }
}

/// # Safety
///
/// The `compressor` must be a valid pointer to a properly initialized
/// `Compressor` and not previously finished or dropped.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Compressor_finish(
    compressor: *mut Compressor,
) -> CompressorFinishResult {
    if let Some(compressor) = compressor.as_mut() {
        CompressorFinishResult::from(compressor.finish())
    } else {
        CompressorFinishResult::Err(ProfileError::InvalidInput)
    }
}

/// # Safety
///
/// The `compressor` must be a valid pointer that was returned by
/// `ddog_prof_Compressor_new` and not previously dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Compressor_drop(
    compressor: *mut Compressor,
) {
    ptr::drop_in_place(compressor);
}
