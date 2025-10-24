// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::{c_char, CStr};
use std::sync::Arc;

use datadog_ffe::rules_based::{Attribute, EvaluationContext, Str};
use ddcommon_ffi::{Handle, ToInner};

/// Creates a new EvaluationContext with the given targeting key
/// 
/// # Safety
/// `targeting_key` must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_new(
    targeting_key: *const c_char,
) -> Handle<EvaluationContext> {
    if targeting_key.is_null() {
        return Handle::empty();
    }

    let key_cstr = match CStr::from_ptr(targeting_key).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let key = Str::from(key_cstr.to_string());
    let attributes = Arc::new(HashMap::<Str, Attribute>::new());
    let context = EvaluationContext::new(key, attributes);

    Handle::from(context)
}

/// Creates a new EvaluationContext with the given targeting key and a single string attribute
/// 
/// # Safety
/// `targeting_key`, `attr_name`, and `attr_value` must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_new_with_attribute(
    targeting_key: *const c_char,
    attr_name: *const c_char,
    attr_value: *const c_char,
) -> Handle<EvaluationContext> {
    if targeting_key.is_null() || attr_name.is_null() || attr_value.is_null() {
        return Handle::empty();
    }

    let key_str = match CStr::from_ptr(targeting_key).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let name_str = match CStr::from_ptr(attr_name).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let value_str = match CStr::from_ptr(attr_value).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let key = Str::from(key_str.to_string());
    let mut attributes = HashMap::<Str, Attribute>::new();
    attributes.insert(
        Str::from(name_str.to_string()),
        Attribute::from(value_str.to_string()),
    );
    let attributes = Arc::new(attributes);
    let context = EvaluationContext::new(key, attributes);

    Handle::from(context)
}

/// Frees an EvaluationContext
/// 
/// # Safety
/// `context` must be a valid EvaluationContext handle created by `ddog_ffe_evaluation_context_new`
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_drop(
    mut context: *mut Handle<EvaluationContext>,
) {
    drop(context.take());
}
