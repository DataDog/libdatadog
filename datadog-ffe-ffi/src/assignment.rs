// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use anyhow::ensure;
use function_name::named;

use datadog_ffe::rules_based::{get_assignment, now, Assignment, Configuration, EvaluationContext};
use ddcommon_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};

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
) -> Result<Handle<Assignment>> {
    wrap_with_ffi_result!({
        ensure!(!flag_key.is_null(), "flag_key must not be NULL");

        let config = config.to_inner_mut()?;
        let context = context.to_inner_mut()?;
        let flag_key = CStr::from_ptr(flag_key).to_str()?;

        let assignment_result = get_assignment(Some(config), flag_key, context, None, now())?;

        let handle = if let Some(assignment) = assignment_result {
            Handle::from(assignment)
        } else {
            Handle::empty()
        };

        Ok(handle)
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
