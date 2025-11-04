// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr, CString};

use anyhow::ensure;
use function_name::named;

use datadog_ffe::rules_based::{get_assignment, now, Assignment, AssignmentValue, AssignmentReason, Configuration, EvaluationContext};
use ddcommon_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};

#[repr(C)]
pub struct ResolutionDetails {
    pub value: Option<AssignmentValue>,
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

impl ResolutionDetails {
    fn empty(reason: Reason) -> Self {
        Self {
            value: None,
            error_code: None,
            error_message: std::ptr::null(),
            reason: Some(reason),
            variant: std::ptr::null(),
            allocation_key: std::ptr::null(),
            do_log: false,
        }
    }
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
            Ok(Some(assignment)) => ResolutionDetails {
                value: Some(assignment.value),
                error_code: None,
                error_message: std::ptr::null(),
                reason: Some(assignment.reason.into()),
                variant: CString::new(assignment.variation_key.as_str()).unwrap().into_raw(),
                allocation_key: CString::new(assignment.allocation_key.as_str()).unwrap().into_raw(),
                do_log: assignment.do_log,
            },
            Ok(None) => ResolutionDetails::empty(Reason::Default),
            Err(_evaluation_error) => ResolutionDetails::empty(Reason::Error),
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
