// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for runtime callback registration
//!
//! This module provides C-compatible FFI bindings for registering runtime-specific
//! crash callbacks that can provide stack traces for dynamic languages.
use datadog_crashtracker::{
    get_registered_callback_type, get_registered_runtime_type_ptr, is_runtime_callback_registered,
    register_runtime_stack_callback, CallbackError, RuntimeStackCallback,
    RuntimeStackCallbackContext, RuntimeStackFrame,
};

#[cfg(test)]
use datadog_crashtracker::{clear_runtime_callback, get_registered_runtime_type};
use std::ffi::c_char;

/// Result type for runtime callback registration
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackResult {
    Ok,
    AlreadyRegistered,
    NullCallback,
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
    context: *mut RuntimeStackCallbackContext,
) -> CallbackResult {
    match register_runtime_stack_callback(callback, context) {
        Ok(()) => CallbackResult::Ok,
        Err(e) => e.into(),
    }
}

/// Check if a runtime callback is currently registered
///
/// Returns true if a callback is registered, false otherwise
#[no_mangle]
pub extern "C" fn ddog_crasht_is_runtime_callback_registered() -> bool {
    is_runtime_callback_registered()
}

/// Get the runtime type from the currently registered callback context
///
/// Returns a pointer to a null-terminated C string containing the runtime type,
/// or null if no callback is registered or the runtime type is unavailable.
///
/// # Safety
/// - The returned pointer is valid only while the callback remains registered
/// - The caller should not free the returned pointer
/// - The returned string should be copied if it needs to persist beyond callback lifetime
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_get_registered_runtime_type() -> *const c_char {
    get_registered_runtime_type_ptr()
}

/// Get the callback type from the currently registered callback context
///
/// Returns a pointer to a null-terminated C string containing the callback type,
/// or null if no callback is registered or the callback type is unavailable.
///
/// # Safety
/// - The returned pointer is valid only while the callback remains registered
/// - The caller should not free the returned pointer
/// - The returned string should be copied if it needs to persist beyond callback lifetime
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_get_registered_callback_type() -> *const c_char {
    get_registered_callback_type()
}

/// Runtime-specific stack frame representation for FFI
///
/// This struct is used to pass runtime stack frame information from lanaguge
/// runtimes to the crash tracker during crash handling.
#[repr(C)]
#[derive(Debug, Clone)]
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
        _emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
        _context: *mut RuntimeStackCallbackContext,
    ) {
        let function_name = CString::new("test_function").unwrap();
        let file_name = CString::new("test.rb").unwrap();
        let class_name = CString::new("TestClass").unwrap();

        // Create the internal RuntimeStackFrame directly - no conversion needed
        // since both RuntimeStackFrame and ddog_RuntimeStackFrame have identical layouts
        let frame = RuntimeStackFrame {
            function_name: function_name.as_ptr(),
            file_name: file_name.as_ptr(),
            line_number: 42,
            column_number: 10,
            class_name: class_name.as_ptr(),
            module_name: ptr::null(),
        };

        emit_frame(&frame as *const RuntimeStackFrame);
    }

    #[test]
    fn test_ffi_callback_registration() {
        unsafe {
            // Ensure clean state at start
            clear_runtime_callback();

            // Use CString to ensure proper null termination
            let runtime_type_cstring = CString::new("ruby").unwrap();
            let callback_type_cstring = CString::new("frame").unwrap();

            let context = RuntimeStackCallbackContext {
                runtime_type: runtime_type_cstring.as_ptr(),
                callback_type: callback_type_cstring.as_ptr(),
            };

            // Test that no callback is initially registered
            assert!(!ddog_crasht_is_runtime_callback_registered());
            assert_eq!(get_registered_runtime_type(), None);

            // Test successful registration
            let mut context = context;
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                &mut context as *mut RuntimeStackCallbackContext,
            );

            assert_eq!(result, CallbackResult::Ok);

            // Verify callback is now registered
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Verify we can retrieve the runtime type
            let runtime_type_str = get_registered_runtime_type().unwrap();
            assert_eq!(runtime_type_str, "ruby");

            // Test duplicate registration fails
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                &mut context as *mut RuntimeStackCallbackContext,
            );
            assert_eq!(result, CallbackResult::AlreadyRegistered);

            // Callback should still be registered after failed duplicate registration
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Clean up - clear the registered callback for subsequent tests
            clear_runtime_callback();
        }
    }

    #[test]
    fn test_context_setting_and_querying() {
        unsafe {
            // Clean up any existing callback first
            clear_runtime_callback();

            // Use CString to ensure proper null termination and avoid memory issues
            let runtime_type_cstring = CString::new("python").unwrap();
            let callback_type_cstring = CString::new("frame").unwrap();

            let context = RuntimeStackCallbackContext {
                runtime_type: runtime_type_cstring.as_ptr(),
                callback_type: callback_type_cstring.as_ptr(),
            };

            // Test that no callback is initially registered
            assert!(!ddog_crasht_is_runtime_callback_registered());
            assert_eq!(ddog_crasht_get_registered_runtime_type(), ptr::null());
            assert_eq!(ddog_crasht_get_registered_callback_type(), ptr::null());

            // Register callback with context
            let mut context = context;
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                &mut context as *mut RuntimeStackCallbackContext,
            );

            assert_eq!(result, CallbackResult::Ok);
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Query and verify context information
            let runtime_type_ptr = ddog_crasht_get_registered_runtime_type();
            assert!(!runtime_type_ptr.is_null());
            let runtime_type_cstr = std::ffi::CStr::from_ptr(runtime_type_ptr);
            let runtime_type_str = runtime_type_cstr.to_str().unwrap();
            assert_eq!(runtime_type_str, "python");

            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            assert!(!callback_type_ptr.is_null());
            let callback_type_cstr = std::ffi::CStr::from_ptr(callback_type_ptr);
            let callback_type_str = callback_type_cstr.to_str().unwrap();
            assert_eq!(callback_type_str, "frame");

            // Test multiple queries to ensure consistency
            for i in 0..3 {
                let rt_ptr = ddog_crasht_get_registered_runtime_type();
                let ct_ptr = ddog_crasht_get_registered_callback_type();

                assert!(
                    !rt_ptr.is_null(),
                    "Runtime type pointer null on iteration {}",
                    i
                );
                assert!(
                    !ct_ptr.is_null(),
                    "Callback type pointer null on iteration {}",
                    i
                );

                let rt_str = std::ffi::CStr::from_ptr(rt_ptr).to_str().unwrap();
                let ct_str = std::ffi::CStr::from_ptr(ct_ptr).to_str().unwrap();

                assert_eq!(rt_str, "python", "Runtime type mismatch on iteration {}", i);
                assert_eq!(ct_str, "frame", "Callback type mismatch on iteration {}", i);
            }

            clear_runtime_callback();

            // Verify cleanup worked
            assert!(!ddog_crasht_is_runtime_callback_registered());
            assert_eq!(ddog_crasht_get_registered_runtime_type(), ptr::null());
            assert_eq!(ddog_crasht_get_registered_callback_type(), ptr::null());
        }
    }

    #[test]
    fn test_multiple_context_configurations() {
        unsafe {
            // Test case 1: Ruby + stacktrace_string
            {
                clear_runtime_callback();

                let runtime_type_cstring = CString::new("ruby").unwrap();
                let callback_type_cstring = CString::new("stacktrace_string").unwrap();

                let context = RuntimeStackCallbackContext {
                    runtime_type: runtime_type_cstring.as_ptr(),
                    callback_type: callback_type_cstring.as_ptr(),
                };

                let mut context = context;
                let result = ddog_crasht_register_runtime_stack_callback(
                    test_runtime_callback,
                    &mut context as *mut RuntimeStackCallbackContext,
                );

                assert_eq!(result, CallbackResult::Ok);
                assert!(ddog_crasht_is_runtime_callback_registered());

                let runtime_type_ptr = ddog_crasht_get_registered_runtime_type();
                assert!(!runtime_type_ptr.is_null());
                let runtime_type_str = std::ffi::CStr::from_ptr(runtime_type_ptr).to_str().unwrap();
                assert_eq!(runtime_type_str, "ruby");

                let callback_type_ptr = ddog_crasht_get_registered_callback_type();
                assert!(!callback_type_ptr.is_null());
                let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                    .to_str()
                    .unwrap();
                assert_eq!(callback_type_str, "stacktrace_string");

                clear_runtime_callback();
            }

            // Test case 2: PHP + frame
            {
                clear_runtime_callback();

                let runtime_type_cstring = CString::new("php").unwrap();
                let callback_type_cstring = CString::new("frame").unwrap();

                let context = RuntimeStackCallbackContext {
                    runtime_type: runtime_type_cstring.as_ptr(),
                    callback_type: callback_type_cstring.as_ptr(),
                };

                let mut context = context;
                let result = ddog_crasht_register_runtime_stack_callback(
                    test_runtime_callback,
                    &mut context as *mut RuntimeStackCallbackContext,
                );

                assert_eq!(result, CallbackResult::Ok);

                let runtime_type_ptr = ddog_crasht_get_registered_runtime_type();
                let runtime_type_str = std::ffi::CStr::from_ptr(runtime_type_ptr).to_str().unwrap();
                assert_eq!(runtime_type_str, "php");

                let callback_type_ptr = ddog_crasht_get_registered_callback_type();
                let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                    .to_str()
                    .unwrap();
                assert_eq!(callback_type_str, "frame");

                clear_runtime_callback();
            }
        }
    }

    #[test]
    fn test_null_context_handling() {
        unsafe {
            clear_runtime_callback();

            // Test with null runtime type
            let context_null_runtime = RuntimeStackCallbackContext {
                runtime_type: ptr::null(),
                callback_type: "frame".as_ptr() as *const c_char,
            };

            let mut context = context_null_runtime;
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                &mut context as *mut RuntimeStackCallbackContext,
            );

            // Registration should still succeed even with null runtime_type
            assert_eq!(result, CallbackResult::Ok);
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Runtime type should return null when context has null runtime_type
            assert_eq!(ddog_crasht_get_registered_runtime_type(), ptr::null());

            // Callback type should still be retrievable
            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            assert!(!callback_type_ptr.is_null());
            let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                .to_str()
                .unwrap();
            assert_eq!(callback_type_str, "frame");

            clear_runtime_callback();

            // Test with null callback type
            let context_null_callback = RuntimeStackCallbackContext {
                runtime_type: "ruby".as_ptr() as *const c_char,
                callback_type: ptr::null(),
            };

            let mut context = context_null_callback;
            let result = ddog_crasht_register_runtime_stack_callback(
                test_runtime_callback,
                &mut context as *mut RuntimeStackCallbackContext,
            );

            assert_eq!(result, CallbackResult::Ok);
            assert!(ddog_crasht_is_runtime_callback_registered());

            // Callback type should return null when context has null callback_type
            assert_eq!(ddog_crasht_get_registered_callback_type(), ptr::null());

            // Runtime type should still be retrievable
            let runtime_type_ptr = ddog_crasht_get_registered_runtime_type();
            assert!(!runtime_type_ptr.is_null());
            let runtime_type_str = std::ffi::CStr::from_ptr(runtime_type_ptr).to_str().unwrap();
            assert_eq!(runtime_type_str, "ruby");

            clear_runtime_callback();
        }
    }

    /// Test that simulates the exact behavior of emit_runtime_stack in emitters.rs
    /// This tests that when a user registers a callback with context, the context
    /// is accessible later when we need to determine how to emit the runtime stack
    #[test]
    fn test_emit_runtime_stack_context_accessibility() {
        unsafe {
            clear_runtime_callback();

            // Simulate the emit_runtime_stack function behavior from emitters.rs:
            // fn emit_runtime_stack(w: &mut impl Write) -> Result<(), EmitterError> {
            //     let callback_type = unsafe { get_registered_callback_type() };
            //     if callback_type.is_null() {
            //         return Ok(());
            //     }
            //     let callback_type_str = unsafe { std::ffi::CStr::from_ptr(callback_type) };
            //     let callback_type_str = callback_type_str.to_str().unwrap();
            //     if callback_type_str == "frame" {
            //         emit_runtime_stack_by_frames(w)
            //     } else if callback_type_str == "stacktrace_string" {
            //         emit_runtime_stack_by_stacktrace_string(w)
            //     } else {
            //         Err(EmitterError::InvalidCallbackType)
            //     }
            // }

            // Test case 1: User registers callback with "frame" callback_type
            {
                let runtime_type_cstring = CString::new("python").unwrap();
                let callback_type_cstring = CString::new("frame").unwrap();

                let context = RuntimeStackCallbackContext {
                    runtime_type: runtime_type_cstring.as_ptr(),
                    callback_type: callback_type_cstring.as_ptr(),
                };

                let mut context = context;
                let result = ddog_crasht_register_runtime_stack_callback(
                    test_runtime_callback,
                    &mut context as *mut RuntimeStackCallbackContext,
                );

                assert_eq!(result, CallbackResult::Ok);

                // Simulate the emit_runtime_stack function checking the context
                let callback_type = get_registered_callback_type();
                assert!(
                    !callback_type.is_null(),
                    "Context should be accessible after registration"
                );

                let callback_type_str = std::ffi::CStr::from_ptr(callback_type);
                let callback_type_str = callback_type_str.to_str().unwrap();

                // This is the key test: the emit_runtime_stack function should be able to
                // determine the callback type from the registered context
                assert_eq!(callback_type_str, "frame");

                // Simulate the logic in emit_runtime_stack
                let should_use_frames = callback_type_str == "frame";
                let should_use_stacktrace_string = callback_type_str == "stacktrace_string";
                let is_invalid_type = !should_use_frames && !should_use_stacktrace_string;

                assert!(
                    should_use_frames,
                    "Should route to emit_runtime_stack_by_frames"
                );
                assert!(!should_use_stacktrace_string);
                assert!(!is_invalid_type);

                clear_runtime_callback();
            }

            // Test case 2: User registers callback with "stacktrace_string" callback_type
            {
                let runtime_type_cstring = CString::new("ruby").unwrap();
                let callback_type_cstring = CString::new("stacktrace_string").unwrap();

                let context = RuntimeStackCallbackContext {
                    runtime_type: runtime_type_cstring.as_ptr(),
                    callback_type: callback_type_cstring.as_ptr(),
                };

                let mut context = context;
                let result = ddog_crasht_register_runtime_stack_callback(
                    test_runtime_callback,
                    &mut context as *mut RuntimeStackCallbackContext,
                );

                assert_eq!(result, CallbackResult::Ok);

                // Simulate the emit_runtime_stack function checking the context
                let callback_type = get_registered_callback_type();
                assert!(!callback_type.is_null());

                let callback_type_str = std::ffi::CStr::from_ptr(callback_type);
                let callback_type_str = callback_type_str.to_str().unwrap();

                assert_eq!(callback_type_str, "stacktrace_string");

                // Simulate the logic in emit_runtime_stack
                let should_use_frames = callback_type_str == "frame";
                let should_use_stacktrace_string = callback_type_str == "stacktrace_string";
                let is_invalid_type = !should_use_frames && !should_use_stacktrace_string;

                assert!(!should_use_frames);
                assert!(
                    should_use_stacktrace_string,
                    "Should route to emit_runtime_stack_by_stacktrace_string"
                );
                assert!(!is_invalid_type);

                clear_runtime_callback();
            }

            // Test case 3: User registers callback with invalid callback_type
            {
                let runtime_type_cstring = CString::new("nodejs").unwrap();
                let callback_type_cstring = CString::new("invalid_type").unwrap();

                let context = RuntimeStackCallbackContext {
                    runtime_type: runtime_type_cstring.as_ptr(),
                    callback_type: callback_type_cstring.as_ptr(),
                };

                let mut context = context;
                let result = ddog_crasht_register_runtime_stack_callback(
                    test_runtime_callback,
                    &mut context as *mut RuntimeStackCallbackContext,
                );

                assert_eq!(result, CallbackResult::Ok);

                // Simulate the emit_runtime_stack function checking the context
                let callback_type = get_registered_callback_type();
                assert!(!callback_type.is_null());

                let callback_type_str = std::ffi::CStr::from_ptr(callback_type);
                let callback_type_str = callback_type_str.to_str().unwrap();

                assert_eq!(callback_type_str, "invalid_type");

                // Simulate the logic in emit_runtime_stack
                let should_use_frames = callback_type_str == "frame";
                let should_use_stacktrace_string = callback_type_str == "stacktrace_string";
                let is_invalid_type = !should_use_frames && !should_use_stacktrace_string;

                assert!(!should_use_frames);
                assert!(!should_use_stacktrace_string);
                assert!(is_invalid_type, "Should return InvalidCallbackType error");

                clear_runtime_callback();
            }

            // Test case 4: No callback registered (should return early)
            {
                clear_runtime_callback();

                // Simulate the emit_runtime_stack function checking the context
                let callback_type = get_registered_callback_type();
                assert!(
                    callback_type.is_null(),
                    "Should return null when no callback is registered"
                );

                // This should cause emit_runtime_stack to return Ok(()) early
                let should_return_early = callback_type.is_null();
                assert!(
                    should_return_early,
                    "emit_runtime_stack should return early when no callback is registered"
                );
            }
        }
    }
}
