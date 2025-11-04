// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use datadog_ffe::rules_based::{
    now, Assignment, Configuration, EvaluationContext, EvaluationError, Str,
};

use crate::Handle;

/// Opaque type representing a result of evaluation.
#[allow(unused)]
pub struct ResolutionDetails(Result<Assignment, EvaluationError>);

#[repr(C)]
pub enum FlagType {
    Unknown,
    String,
    Integer,
    Float,
    Boolean,
    Object,
}

#[repr(C)]
pub enum ErrorCode {
    Ok,
    TypeMismatch,
    ParseError,
    FlagNotFound,
    TargetingKeyMissing,
    InvalidContext,
    ProviderNotReady,
    General,
}

#[repr(C)]
pub enum Reason {
    Static,
    Default,
    TargetingMatch,
    Split,
    Disabled,
    Error,
}

/// Evaluates a feature flag.
///
/// # Ownership
///
/// The caller must call `ddog_ffe_assignment_drop` on the returned value to free resources.
///
/// # Safety
/// - `config` must be a valid `Configuration` handle
/// - `flag_key` must be a valid C string
/// - `context` must be a valid `EvaluationContext` handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_get_assignment(
    config: Handle<Configuration>,
    flag_key: *const c_char,
    _expected_type: FlagType,
    context: Handle<EvaluationContext>,
) -> Handle<ResolutionDetails> {
    if flag_key.is_null() {
        return Handle::from(ResolutionDetails(Err(EvaluationError::Internal(
            Str::from_static_str("ddog_ffe_get_assignment: flag_key must not be NULL"),
        ))));
    }

    let config = unsafe { config.as_ref() };
    let context = unsafe { context.as_ref() };

    let Ok(flag_key) = unsafe {
        // SAFETY: we checked that flag_key is not NULL
        CStr::from_ptr(flag_key)
    }
    .to_str() else {
        return Handle::from(ResolutionDetails(Err(EvaluationError::Internal(
            Str::from_static_str("ddog_ffe_get_assignment: flag_key is not a valid UTF-8 string"),
        ))));
    };

    let assignment_result = config.eval_flag(flag_key, context, None, now());

    Handle::from(ResolutionDetails(assignment_result))
}

// TODO: accessors for various data inside ResolutionDetails.

/// Frees an Assignment handle.
///
/// # Safety
/// - `assignment` must be a valid Assignment handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(assignment: *mut Handle<ResolutionDetails>) {
    unsafe { Handle::free(assignment) }
}
