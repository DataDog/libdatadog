// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::{c_char, CStr};
use std::sync::Arc;

use datadog_ffe::rules_based::{Attribute, EvaluationContext, Str};
use ddcommon_ffi::{Handle, ToInner};

/// Represents a key-value pair for attributes
#[repr(C)]
pub struct AttributePair {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// Creates a new EvaluationContext with the given targeting key and attributes
///
/// # Safety
/// - `targeting_key` must be a valid null-terminated C string
/// - `attributes` must point to a valid array of `AttributePair` structs (can be null if
///   attributes_count is 0)
/// - Each `AttributePair.name` and `AttributePair.value` must be valid null-terminated C strings
/// - `attributes_count` must accurately represent the length of the `attributes` array
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_new(
    targeting_key: *const c_char,
    attributes: *const AttributePair,
    attributes_count: usize,
) -> Handle<EvaluationContext> {
    if targeting_key.is_null() || (attributes_count > 0 && attributes.is_null()) {
        return Handle::empty();
    }

    let key_str = match CStr::from_ptr(targeting_key).to_str() {
        Ok(s) => s,
        Err(_) => return Handle::empty(),
    };

    let key = Str::from(key_str.to_string());
    let mut attr_map = HashMap::<Str, Attribute>::new();

    // Process attributes array
    for i in 0..attributes_count {
        let attr_pair = &*attributes.add(i);

        if attr_pair.name.is_null() || attr_pair.value.is_null() {
            continue; // Skip invalid pairs
        }

        let name_str = match CStr::from_ptr(attr_pair.name).to_str() {
            Ok(s) => s,
            Err(_) => continue, // Skip invalid UTF-8
        };

        let value_str = match CStr::from_ptr(attr_pair.value).to_str() {
            Ok(s) => s,
            Err(_) => continue, // Skip invalid UTF-8
        };

        attr_map.insert(
            Str::from(name_str.to_string()),
            Attribute::from(value_str.to_string()),
        );
    }

    let attributes_arc = Arc::new(attr_map);
    let context = EvaluationContext::new(key, attributes_arc);

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
