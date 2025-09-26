// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for runtime callback registration
//!
//! This module provides C-compatible FFI bindings for registering runtime-specific
//! crash callbacks that can provide stack traces for dynamic languages.

use datadog_crashtracker::{
    register_runtime_stack_callback, CallbackError, RuntimeStackCallback, RuntimeStackFrame,
};
use std::ffi::{c_char, c_void};

/// Result type for runtime callback operations
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackResult {
    /// Operation succeeded
    Ok,
    /// A callback is already registered
    AlreadyRegistered,
    /// Null callback function provided
    NullCallback,
    /// An unknown error occurred
    UnknownError,
}

impl From<CallbackError> for CallbackResult {
    fn from(error: CallbackError) -> Self {
        match error {
            CallbackError::AlreadyRegistered => CallbackResult::AlreadyRegistered,
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
/// - `CallbackResult::AlreadyRegistered` if a callback is already registered
/// - `CallbackResult::NullCallback` if the callback function is null
///
/// # Safety
/// - The callback must be signal-safe
/// - The context pointer must remain valid for the lifetime of the process
/// - Only one callback can be registered at a time
///
/// # Example Usage from C
/// ```c
/// static void my_runtime_callback(
///     void (*emit_frame)(const ddog_RuntimeStackFrame*),
///     void* context
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
///     emit_frame(&frame);
/// }
///
/// ddog_CallbackResult result = ddog_crasht_register_runtime_stack_callback(
///     my_runtime_callback,
///     NULL
/// );
/// ```
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_register_runtime_stack_callback(
    callback: RuntimeStackCallback,
    context: *mut c_void,
) -> CallbackResult {
    match register_runtime_stack_callback(callback, context) {
        Ok(()) => CallbackResult::Ok,
        Err(e) => e.into(),
    }
}

/// Runtime-specific stack frame representation for FFI
///
/// This struct is used to pass runtime stack frame information from language
/// runtimes to the crashtracker during crash handling.
#[repr(C)]
#[derive(Debug)]
#[allow(non_camel_case_types)]
pub struct ddog_RuntimeStackFrame {
    /// Function/method name (null-terminated C string)
    pub function_name: *const c_char,
    /// Source file name (null-terminated C string)
    pub file_name: *const c_char,
    /// Line number in source file
    pub line_number: u32,
    /// Column number in source file (0 if unknown)
    pub column_number: u32,
    /// Class name for OOP languages (nullable)
    pub class_name: *const c_char,
    /// Module/namespace name (nullable)
    pub module_name: *const c_char,
}

impl From<ddog_RuntimeStackFrame> for RuntimeStackFrame {
    fn from(frame: ddog_RuntimeStackFrame) -> Self {
        // Direct mapping since both use *const c_char
        Self {
            function_name: frame.function_name,
            file_name: frame.file_name,
            line_number: frame.line_number,
            column_number: frame.column_number,
            class_name: frame.class_name,
            module_name: frame.module_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    unsafe extern "C" fn test_runtime_callback(
        emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
        _context: *mut c_void,
    ) {
        let function_name = CString::new("test_function").unwrap();
        let file_name = CString::new("test.rb").unwrap();
        let class_name = CString::new("TestClass").unwrap();

        let frame = RuntimeStackFrame {
            function_name: function_name.as_ptr(),
            file_name: file_name.as_ptr(),
            line_number: 42,
            column_number: 10,
            class_name: class_name.as_ptr(),
            module_name: ptr::null(),
        };

        emit_frame(&frame);
    }

    #[test]
    fn test_ffi_callback_registration() {
        unsafe {
            // Test successful registration
            let result =
                ddog_crasht_register_runtime_stack_callback(test_runtime_callback, ptr::null_mut());
            assert_eq!(result, CallbackResult::Ok);

            // Test duplicate registration fails
            let result =
                ddog_crasht_register_runtime_stack_callback(test_runtime_callback, ptr::null_mut());
            assert_eq!(result, CallbackResult::AlreadyRegistered);
        }
    }
}
