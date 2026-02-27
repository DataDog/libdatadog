// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common_ffi::VoidResult;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value, AnyValue, KeyValue, ProcessContext,
};
use std::{ffi::CStr, os::raw::c_char};

fn mk_key_value(key: &str, value: any_value::Value) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue { value: Some(value) }),
        key_ref: 0,
    }
}

/// Allocates and returns a pointer to a new, empty [`ProcessContext`] on the heap.
///
/// The caller is responsible for calling [`ddog_otel_process_ctxt_free`] to deallocate the memory.
///
/// # Returns
///
/// A non-null pointer to a newly allocated [`ProcessContext`] instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_new() -> *mut ProcessContext {
    Box::into_raw(Box::new(ProcessContext::default()))
}

/// Frees a [`ProcessContext`] instance previously allocated with [`ddog_otel_process_ctxt_new`].
///
/// # Safety
///
/// - `ctxt` must be a valid pointer previously returned by [`ddog_otel_process_ctxt_new`]
/// - `ctxt` must NOT have been already freed by this function (double-free)
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_free(ctxt: *mut ProcessContext) {
    if !ctxt.is_null() {
        // Safety: `ctxt` is required to have come from `ddog_otel_process_ctxt_new`, which
        // allocates through `Box`
        let _ = unsafe { Box::from_raw(ctxt) };
    }
}

/// Sets a string attribute on the resource of a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If any of the provided strings is not valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` and `value` must point to null-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_resource_attr_str(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: *const c_char,
) {
    if ctxt.is_null() || key.is_null() || value.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `value_str` to be a valid UTF8 C string
    let Ok(value_str) = (unsafe { CStr::from_ptr(value).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };

    ctxt.resource
        .get_or_insert_default()
        .attributes
        .push(mk_key_value(
            key_str,
            any_value::Value::StringValue(value_str.to_owned()),
        ));
}

/// Sets an integer attribute on the resource of a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_resource_attr_int(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: i64,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };
    ctxt.resource
        .get_or_insert_default()
        .attributes
        .push(mk_key_value(key_str, any_value::Value::IntValue(value)));
}

/// Sets a double attribute on the resource of a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_resource_attr_double(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: f64,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };
    ctxt.resource
        .get_or_insert_default()
        .attributes
        .push(mk_key_value(key_str, any_value::Value::DoubleValue(value)));
}

/// Sets a boolean attribute on the resource of a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_resource_attr_bool(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: bool,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };
    ctxt.resource
        .get_or_insert_default()
        .attributes
        .push(mk_key_value(key_str, any_value::Value::BoolValue(value)));
}

/// Sets a string extra attribute on a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If any of the provided strings is not valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained from
/// [`ddog_otel_process_ctxt_new`].
/// - `key` and `value` must point to null-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_extra_attr_str(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: *const c_char,
) {
    if ctxt.is_null() || key.is_null() || value.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `value_str` to be a valid UTF8 C string
    let Ok(value_str) = (unsafe { CStr::from_ptr(value).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };

    ctxt.extra_attributes.push(mk_key_value(
        key_str,
        any_value::Value::StringValue(value_str.to_owned()),
    ));
}

/// Sets an integer extra attribute on a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_extra_attr_int(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: i64,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };

    ctxt.extra_attributes
        .push(mk_key_value(key_str, any_value::Value::IntValue(value)));
}

/// Sets a double extra attribute on a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_extra_attr_double(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: f64,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };

    ctxt.extra_attributes
        .push(mk_key_value(key_str, any_value::Value::DoubleValue(value)));
}

/// Sets a boolean extra attribute on a [`ProcessContext`].
///
/// A given attribute must be set at most once for a given process context object, or the
/// interpretation by the reader is not well-defined.
///
/// If `key` isn't valid UTF8, this function does nothing.
///
/// # Safety
///
/// - `ctxt` must be a non-null pointer to a [`ProcessContext`] object obtained form
/// [`ddog_otel_process_ctxt_new`].
/// - `key` must point to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_set_extra_attr_bool(
    ctxt: *mut ProcessContext,
    key: *const c_char,
    value: bool,
) {
    if ctxt.is_null() || key.is_null() {
        return;
    }

    // Safety: this function requires `key_str` to be a valid UTF8 C string
    let Ok(key_str) = (unsafe { CStr::from_ptr(key).to_str() }) else {
        return;
    };

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &mut *ctxt };

    ctxt.extra_attributes
        .push(mk_key_value(key_str, any_value::Value::BoolValue(value)));
}

/// Publishes or updates the process context so it is visible to external readers via a
/// named memory mapping.
///
/// If this is the first call to [`ddog_otel_process_ctxt_publish`], or if
/// [`ddog_otel_process_ctxt_unpublish`] was called last, a new mapping is created following the
/// Publish protocol.
/// Otherwise, the mapping is updated following the Update protocol.
///
/// The [`ProcessContext`] pointed to by `ctxt` is encoded directly into the mapping. The original
/// data is left unchanged. The pointer remains valid, is still owned by the caller after this
/// call, and must be freed accordingly. The context pointed to by `ctxt` can be freed as soon as
/// this function returns.
///
/// # Safety
///
/// - `ctxt` must be a valid non-null pointer to [`ProcessContext`].
#[cfg(target_os = "linux")]
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_publish(ctxt: *const ProcessContext) -> VoidResult {
    if ctxt.is_null() {
        return VoidResult::Err(
            anyhow::anyhow!("null pointer passed to ddog_otel_process_ctxt_publish").into(),
        );
    }

    // Safety: this function requires `ctxt` to be a valid non-null pointer to a
    // `ProcessContext`
    let ctxt = unsafe { &*ctxt };
    libdd_library_config::otel_process_ctxt::linux::publish(ctxt).into()
}

/// Unmaps the memory region used to share the process context and closes the associated
/// file descriptor, if any. If no context has been published, this is a no-op.
///
/// A subsequent call to [`ddog_otel_process_ctxt_publish`] will create a new mapping.
#[cfg(target_os = "linux")]
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_process_ctxt_unpublish() -> VoidResult {
    libdd_library_config::otel_process_ctxt::linux::unpublish().into()
}
