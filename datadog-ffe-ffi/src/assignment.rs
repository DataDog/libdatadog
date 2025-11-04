// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, c_uchar, CStr};

use datadog_ffe::rules_based::{
    now, Assignment, AssignmentValue, Configuration, EvaluationContext, EvaluationError, Str,
    VariationType,
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

impl From<VariationType> for FlagType {
    fn from(value: VariationType) -> Self {
        match value {
            VariationType::String => FlagType::String,
            VariationType::Integer => FlagType::Integer,
            VariationType::Numeric => FlagType::Float,
            VariationType::Boolean => FlagType::Boolean,
            VariationType::Json => FlagType::Object,
        }
    }
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

#[repr(C)]
pub enum VariantValue {
    /// Evaluation did not produce any value.
    None,
    String(*const c_uchar),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(*const c_char),
}

/// Get value produced by evaluation.
///
/// # Ownership
///
/// The returned `VariantValue` borrows from `assignment`. It must not be used after `assignment` is
/// freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_get_value(
    assignment: Handle<ResolutionDetails>,
) -> VariantValue {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => match &assignment.value {
            AssignmentValue::String(s) => VariantValue::String(s.as_ptr()),
            AssignmentValue::Integer(v) => VariantValue::Integer(*v),
            AssignmentValue::Float(v) => VariantValue::Float(*v),
            AssignmentValue::Boolean(v) => VariantValue::Boolean(*v),
            AssignmentValue::Json(_value) => todo!("make AssignmentValue hold onto raw json value"),
        },
        _ => VariantValue::None,
    }
}

/// Get variant key produced by evaluation. Returns `NULL` if evaluation did not produce any value.
///
/// # Ownership
///
/// The returned string borrows from `assignment`. It must not be used after `assignment` is
/// freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_get_variant(
    assignment: Handle<ResolutionDetails>,
) -> *const c_uchar {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => assignment.variation_key.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// Get allocation key produced by evaluation. Returns `NULL` if evaluation did not produce any
/// value.
///
/// # Ownership
///
/// The returned string borrows from `assignment`. It must not be used after `assignment` is
/// freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_get_allocation_key(
    assignment: Handle<ResolutionDetails>,
) -> *const c_uchar {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => assignment.allocation_key.as_ptr(),
        _ => std::ptr::null(),
    }
}

// TODO: add accessors for various data inside ResolutionDetails.

/// Frees an Assignment handle.
///
/// # Safety
/// - `assignment` must be a valid Assignment handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(assignment: *mut Handle<ResolutionDetails>) {
    unsafe { Handle::free(assignment) }
}
