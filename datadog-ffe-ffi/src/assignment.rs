// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use anyhow::ensure;
use function_name::named;

use datadog_ffe::rules_based::{
    get_assignment, now, Assignment, AssignmentReason, AssignmentValue, Configuration,
    EvaluationContext, EvaluationError,
};
use ddcommon_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};

#[repr(C)]
pub struct ResolutionDetails {
    pub value_type: *const c_char, // "STRING", "INTEGER", "FLOAT", "BOOLEAN", "JSON", or NULL
    pub value_string: *const c_char, // String representation of the value, or NULL
    pub error_code: Option<ErrorCode>,
    pub error_message: *const c_char, // C-compatible string
    pub reason: Option<Reason>,
    pub variant: *const c_char,
    pub allocation_key: *const c_char,
    pub do_log: bool,
}

#[repr(C)]
pub enum ErrorCode {
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


/// Helper function to safely create a CString, replacing null bytes with a placeholder
fn safe_cstring(input: &str) -> *const c_char {
    use std::ffi::CString;
    
    // Replace null bytes with a placeholder to avoid CString::new() panics
    let sanitized = input.replace('\0', "\\0");
    
    match CString::new(sanitized) {
        Ok(cstring) => cstring.into_raw(),
        Err(_) => {
            // Fallback to a static error message if somehow still fails
            // This should never happen since we sanitized the input, but safety first
            static FALLBACK: &[u8] = b"invalid_string\0";
            FALLBACK.as_ptr() as *const c_char
        }
    }
}

fn convert_evaluation_error(error: &EvaluationError) -> ErrorCode {
    use datadog_ffe::rules_based::EvaluationError;

    match error {
        EvaluationError::TypeMismatch { .. } => ErrorCode::TypeMismatch,
        EvaluationError::UnexpectedConfigurationError => ErrorCode::General,
        // Handle any future variants that might be added
        _ => ErrorCode::General,
    }
}

fn convert_assignment_value(value: &AssignmentValue) -> (*const c_char, *const c_char) {
    use datadog_ffe::rules_based::AssignmentValue;

    let (type_name, value_string) = match value {
        AssignmentValue::String(s) => ("STRING", s.as_str().to_owned()),
        AssignmentValue::Integer(i) => ("INTEGER", i.to_string()),
        AssignmentValue::Float(f) => ("FLOAT", f.to_string()),
        AssignmentValue::Boolean(b) => ("BOOLEAN", b.to_string()),
        AssignmentValue::Json(j) => ("JSON", j.to_string()),
    };

    let type_str = safe_cstring(type_name);
    let value_str = safe_cstring(&value_string);
    (type_str, value_str)
}

impl From<AssignmentReason> for Reason {
    fn from(reason: AssignmentReason) -> Self {
        match reason {
            AssignmentReason::Static => Reason::Static,
            AssignmentReason::TargetingMatch => Reason::TargetingMatch,
            AssignmentReason::Split => Reason::Split,
        }
    }
}

/// Evaluates a feature flag.
///
/// # Safety
/// - `config` must be a valid Configuration handle pointer
/// - `context` must be a valid EvaluationContext handle pointer
/// - `flag_key` must be a valid null-terminated C string
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_ffe_get_assignment(
    mut config: *mut Handle<Configuration>,
    flag_key: *const c_char,
    mut context: *mut Handle<EvaluationContext>,
) -> Result<Handle<ResolutionDetails>> {
    wrap_with_ffi_result!({
        ensure!(!flag_key.is_null(), "flag_key must not be NULL");

        let config = config.to_inner_mut()?;
        let context = context.to_inner_mut()?;
        let flag_key = CStr::from_ptr(flag_key).to_str()?;

        let assignment_result = get_assignment(Some(config), flag_key, context, None, now());

        let resolution_details = match assignment_result {
            Ok(Some(assignment)) => {
                let (value_type, value_string) = convert_assignment_value(&assignment.value);
                ResolutionDetails {
                    value_type,
                    value_string,
                    error_code: None,
                    error_message: std::ptr::null(),
                    reason: Some(assignment.reason.into()),
                    variant: safe_cstring(assignment.variation_key.as_str()),
                    allocation_key: safe_cstring(assignment.allocation_key.as_str()),
                    do_log: assignment.do_log,
                }
            }
            Ok(None) => {
                // Return empty handle to signal no assignment found
                return Ok(Handle::empty());
            }
            Err(evaluation_error) => ResolutionDetails {
                value_type: std::ptr::null(),
                value_string: std::ptr::null(),
                error_code: Some(convert_evaluation_error(&evaluation_error)),
                error_message: safe_cstring(&evaluation_error.to_string()),
                reason: Some(Reason::Error),
                variant: std::ptr::null(),
                allocation_key: std::ptr::null(),
                do_log: false,
            },
        };

        Ok(Handle::from(resolution_details))
    })
}

/// Frees an Assignment handle
///
/// # Safety
/// `assignment` must be a valid Assignment handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(mut assignment: *mut Handle<Assignment>) {
    drop(assignment.take());
}
