// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::{c_char, CStr};
use std::sync::Arc;

use datadog_ffe::rules_based::{Attribute, EvaluationContext, Str};

use crate::Handle;

/// Represents a key-value pair for attributes.
///
/// # Safety
/// - `name` must be a valid C string.
#[repr(C)]
pub struct AttributePair {
    pub name: *const c_char,
    pub value: AttributeValue,
}

/// # Safety
/// - `string` must be a valid C string.
#[repr(C)]
pub enum AttributeValue {
    String(*const c_char),
    Number(f64),
    Boolean(bool),
}

/// Creates a new EvaluationContext with the given targeting key and attributes.
///
/// # Ownership
///
/// `ddog_ffe_evaluation_context_drop` must be called on the result value to free resources.
///
/// # Safety
/// - `targeting_key` must be a valid C string.
/// - `attributes` must point to a valid array of valid `AttributePair` structs (can be null if
///   `attributes_count` is 0)
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_new(
    targeting_key: *const c_char,
    attributes: *const AttributePair,
    attributes_count: usize,
) -> Handle<EvaluationContext> {
    let targeting_key = if targeting_key.is_null() {
        Str::from_static_str("")
    } else {
        match unsafe { CStr::from_ptr(targeting_key).to_str() } {
            Ok(s) => Str::from(s),
            Err(_) => Str::from_static_str(""),
        }
    };

    let attributes = if attributes.is_null() {
        HashMap::new()
    } else {
        unsafe { std::slice::from_raw_parts(attributes, attributes_count) }
            .into_iter()
            .filter_map(|attr_pair| {
                if attr_pair.name.is_null() {
                    return None; // Skip invalid pairs
                }

                let name_str = unsafe { CStr::from_ptr(attr_pair.name) }.to_str().ok()?;

                let attribute: Attribute = match attr_pair.value {
                    AttributeValue::String(s) => unsafe { CStr::from_ptr(s) }.to_str().ok()?.into(),
                    AttributeValue::Number(v) => v.into(),
                    AttributeValue::Boolean(v) => v.into(),
                };

                Some((Str::from(name_str), attribute))
            })
            .collect()
    };

    Handle::from(EvaluationContext::new(targeting_key, Arc::new(attributes)))
}

/// Frees an EvaluationContext
///
/// # Safety
/// `context` must be a valid EvaluationContext handle created by `ddog_ffe_evaluation_context_new`
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_evaluation_context_drop(context: *mut Handle<EvaluationContext>) {
    unsafe { Handle::free(context) };
}
