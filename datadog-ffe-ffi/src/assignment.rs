// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    ffi::{c_char, c_uchar, CStr},
    marker::PhantomData,
};

use datadog_ffe::rules_based::{
    now, Assignment, AssignmentValue, Configuration, EvaluationContext, EvaluationError, Str,
    VariationType,
};
use ddcommon_ffi::CharSlice;

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
    String(BorrowedStr),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(BorrowedStr),
}

/// A string that has been borrowed. Beware that it is NOT nul-terminated!
///
/// # Ownership
///
/// This string is non-owning. You must not free `ptr`.
///
/// # Safety
///
/// - The string is not NUL-terminated, it can only be used with API that accept the len as an
///   additional parameter.
/// - The value must not be used after the value it borrowed from has been moved, modified, or
///   freed.
#[repr(C)]
pub struct BorrowedStr {
    /// May be NULL if `len` is `0`.
    pub ptr: *const u8,
    pub len: usize,
}

impl<'a> BorrowedStr {
    /// Borrow string from `s`.
    ///
    /// # Safety
    ///
    /// - The returned value must non outlive `s`.
    /// - `s` must not be modified while `BorrowedStr` is alive.
    #[inline]
    unsafe fn new(s: &str) -> BorrowedStr {
        BorrowedStr {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    #[inline]
    const fn empty() -> BorrowedStr {
        BorrowedStr {
            ptr: std::ptr::null(),
            len: 0,
        }
    }
}

/// Get value produced by evaluation.
///
/// # Ownership
///
/// The returned `VariantValue` borrows from `assignment`. It must not be used after `assignment` is
/// freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_get_value<'a>(
    assignment: Handle<ResolutionDetails>,
) -> VariantValue {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => match &assignment.value {
            AssignmentValue::String(s) => {
                VariantValue::String(unsafe {
                    // SAFETY: caller is required to not use return value after freeing
                    // `assignment`.
                    BorrowedStr::new(s.as_str())
                })
            }
            AssignmentValue::Integer(v) => VariantValue::Integer(*v),
            AssignmentValue::Float(v) => VariantValue::Float(*v),
            AssignmentValue::Boolean(v) => VariantValue::Boolean(*v),
            AssignmentValue::Json { value: _, raw } => {
                VariantValue::Object(unsafe {
                    // SAFETY: caller is required to not use return value after freeing
                    // `assignment`.
                    BorrowedStr::new(raw.get())
                })
            }
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
) -> BorrowedStr {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => unsafe {
            // SAFETY: caller is required to not use return value after freeing
            // `assignment`.
            BorrowedStr::new(&assignment.variation_key)
        },
        _ => BorrowedStr::empty(),
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
) -> BorrowedStr {
    match unsafe { assignment.as_ref() } {
        ResolutionDetails(Ok(assignment)) => unsafe {
            // SAFETY: caller is required to not use return value after freeing
            // `assignment`.
            BorrowedStr::new(assignment.allocation_key.as_str())
        },
        _ => BorrowedStr::empty(),
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
