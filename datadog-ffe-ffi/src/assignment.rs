// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};
use std::mem::MaybeUninit;

use datadog_ffe::rules_based::{get_assignment, now, Assignment, Configuration, EvaluationContext};
use ddcommon_ffi::{Handle, ToInner, VoidResult};

use crate::error::ffe_error;

/// Evaluates a feature flag and returns success/failure via VoidResult
/// If successful, writes the assignment to the output parameter
///
/// # Safety
/// - `config` must be a valid Configuration handle
/// - `context` must be a valid EvaluationContext handle  
/// - `flag_key` must be a valid null-terminated C string
/// - `assignment_out` must point to valid uninitialized memory for a Handle<Assignment>
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_get_assignment(
    mut config: *mut Handle<Configuration>,
    flag_key: *const c_char,
    mut context: *mut Handle<EvaluationContext>,
    assignment_out: *mut MaybeUninit<Handle<Assignment>>,
) -> VoidResult {
    if flag_key.is_null() {
        return VoidResult::Err(ffe_error("flag_key cannot be null"));
    }
    if assignment_out.is_null() {
        return VoidResult::Err(ffe_error("assignment_out cannot be null"));
    }

    let config_ref = match config.to_inner_mut() {
        Ok(c) => c,
        Err(e) => return VoidResult::Err(ffe_error(&e.to_string())),
    };

    let context_ref = match context.to_inner_mut() {
        Ok(c) => c,
        Err(e) => return VoidResult::Err(ffe_error(&e.to_string())),
    };

    let flag_key_str = match CStr::from_ptr(flag_key).to_str() {
        Ok(s) => s,
        Err(_) => return VoidResult::Err(ffe_error("flag_key must be valid UTF-8")),
    };

    let assignment_result =
        get_assignment(Some(config_ref), flag_key_str, context_ref, None, now());

    match assignment_result {
        Ok(Some(assignment)) => {
            assignment_out.write(MaybeUninit::new(Handle::from(assignment)));
            VoidResult::Ok
        }
        Ok(None) => {
            assignment_out.write(MaybeUninit::new(Handle::empty()));
            VoidResult::Ok
        }
        Err(_) => VoidResult::Err(ffe_error("assignment evaluation failed")),
    }
}

/// Frees an Assignment handle
///
/// # Safety
/// `assignment` must be a valid Assignment handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(mut assignment: *mut Handle<Assignment>) {
    drop(assignment.take());
}
