// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for runtime callback registration
//!
//! This module provides C-compatible FFI bindings for registering runtime-specific
//! crash callbacks that can provide stack traces for dynamic languages.
use datadog_crashtracker::{
    get_registered_callback_type_ptr, is_runtime_callback_registered,
    register_runtime_stack_callback, CallbackError, CallbackType, RuntimeStackCallback,
};

// Re-export the enums for C/C++ consumers
pub use datadog_crashtracker::CallbackType as ddog_CallbackType;

pub use datadog_crashtracker::RuntimeStackFrame as ddog_RuntimeStackFrame;

/// Result type for runtime callback registration
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackResult {
    Ok,
    NullCallback,
    UnknownError,
}

impl From<CallbackError> for CallbackResult {
    fn from(error: CallbackError) -> Self {
        match error {
            CallbackError::NullCallback => CallbackResult::NullCallback,
        }
    }
}

/// Register a runtime stack collection callback
///
/// This function allows language runtimes to register a callback that will be invoked
/// during crash handling to collect runtime-specific stack traces.
///
/// # Arguments
/// - `callback`: The callback function to invoke during crashes
/// - `context`: User-provided context pointer passed to the callback
///
/// # Returns
/// - `CallbackResult::Ok` if registration succeeds
/// - `CallbackResult::NullCallback` if the callback function is null
///
/// # Safety
/// - The callback must be signal-safe
/// - The context pointer must remain valid for the lifetime of the process
/// - Only one callback can be registered at a time
/// - The callback must be registered once on CrashTracker initialization, before any crash occurs
///
/// # Example Usage from C
/// ```c
/// static void my_runtime_callback(
///     void (*emit_frame)(const ddog_RuntimeStackFrame*),
///     void (*emit_stacktrace_string)(const char*),
///     void* writer_ctx
/// ) {
///     // Collect runtime frames and call emit_frame for each one
///     ddog_RuntimeStackFrame frame = {
///         .function_name = "my_function",
///         .file_name = "script.rb",
///         .line_number = 42,
///         .column_number = 10,
///         .class_name = "MyClass",
///         .module_name = NULL
///     };
///     emit_frame(writer_ctx, &frame);
/// }
///
///
/// ddog_CallbackResult result = ddog_crasht_register_runtime_stack_callback(
///     my_runtime_callback,
///     CallbackType::Frame,
/// );
/// ```
/// Register a runtime stack collection callback using type-safe enums
///
/// This function provides compile-time safety by using enums instead of strings
/// for runtime and callback types.
///
/// # Arguments
/// - `callback`: The callback function to invoke during crashes
/// - `callback_type`: Callback type enum (Frame, StacktraceString)
///
/// # Returns
/// - `CallbackResult::Ok` if registration succeeds (replaces any existing callback)
/// - `CallbackResult::NullCallback` if the callback function is null
///
/// # Safety
/// - The callback must be signal-safe
/// - Only one callback can be registered at a time (this replaces any existing one)
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_register_runtime_stack_callback(
    callback: RuntimeStackCallback,
    callback_type: CallbackType,
) -> CallbackResult {
    match register_runtime_stack_callback(callback, callback_type) {
        Ok(()) => CallbackResult::Ok,
        Err(e) => e.into(),
    }
}

/// Check if a runtime callback is currently registered
///
/// Returns true if a callback is registered, false otherwise
///
/// # Safety
/// This function is safe to call at any time
#[no_mangle]
pub extern "C" fn ddog_crasht_is_runtime_callback_registered() -> bool {
    is_runtime_callback_registered()
}

/// Get the callback type from the currently registered callback context
///
/// Returns the callback type C string pointer if a callback with valid context is registered,
/// null pointer otherwise
///
/// # Safety
/// - The returned pointer is valid only while the callback remains registered
/// - The caller should not free the returned pointer
/// - The returned string should be copied if it needs to persist beyond callback lifetime
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_get_registered_callback_type() -> *const std::ffi::c_char {
    get_registered_callback_type_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_crashtracker::{clear_runtime_callback, RuntimeStackFrame};
    use std::ffi::{c_char, c_void, CString};
    use std::ptr;
    use std::sync::Mutex;

    // Use a mutex to ensure tests run sequentially to avoid race conditions
    // with the global static variable
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn test_runtime_callback(
        emit_frame: unsafe extern "C" fn(*mut c_void, *const RuntimeStackFrame),
        _emit_stacktrace_string: unsafe extern "C" fn(*mut c_void, *const c_char),
        writer_ctx: *mut c_void,
    ) {
        let function_name = CString::new("test_function").unwrap();
        let file_name = CString::new("test.rb").unwrap();
        let class_name = CString::new("TestClass").unwrap();

        // Create the internal RuntimeStackFrame directly; no conversion needed
        // since both RuntimeStackFrame and ddog_RuntimeStackFrame have identical layouts
        let frame = RuntimeStackFrame {
            function_name: function_name.as_ptr(),
            file_name: file_name.as_ptr(),
            line_number: 42,
            column_number: 10,
            class_name: class_name.as_ptr(),
            module_name: ptr::null(),
        };

        emit_frame(writer_ctx, &frame);
    }

    #[test]
    fn test_ffi_callback_registration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            // Ensure clean state at start
            clear_runtime_callback();

            // Test that no callback is initially registered
            assert!(!ddog_crasht_is_runtime_callback_registered());

            // Test successful registration using type-safe enums
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                CallbackType::Frame,
            );

            assert_eq!(result, CallbackResult::Ok);

            // Verify callback is now registered
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Test duplicate registration fails
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                CallbackType::Frame,
            );
            assert_eq!(result, CallbackResult::Ok);

            // Callback should still be registered after successful re-registration
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Clean up - clear the registered callback for subsequent tests
            clear_runtime_callback();
        }
    }

    #[test]
    fn test_enum_based_registration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            clear_runtime_callback();

            // Test that no callback is initially registered
            assert!(!ddog_crasht_is_runtime_callback_registered());

            // Test registration with enum values - Python + StacktraceString
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                CallbackType::StacktraceString,
            );

            assert_eq!(result, CallbackResult::Ok);
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Verify callback type
            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            assert!(!callback_type_ptr.is_null());
            let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                .to_str()
                .unwrap();
            assert_eq!(callback_type_str, "stacktrace_string");

            // Test re-registration with different values - Ruby + Frame
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                CallbackType::Frame,
            );

            assert_eq!(result, CallbackResult::Ok);

            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                .to_str()
                .unwrap();
            assert_eq!(callback_type_str, "frame");

            clear_runtime_callback();
        }
    }
}
