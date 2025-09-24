// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime callback registration system for enhanced crash tracing
//!
//! This module provides APIs for runtime languages (Ruby, Python, PHP, etc.) to register
//! callbacks that can provide runtime-specific stack traces during crash handling.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use thiserror::Error;

/// Runtime-specific stack frame representation suitable for signal-safe context
#[repr(C)]
#[derive(Debug, Clone)]
pub struct RuntimeStackFrame {
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

/// Function signature for runtime stack collection callbacks
///
/// This callback is invoked during crash handling in a signal context, so it must be signal-safe:
/// - No dynamic memory allocation
/// - No mutex operations
/// - No I/O operations
/// - Only async-signal-safe functions
///
/// # Parameters
/// - `emit_frame`: Function to call for each runtime frame
/// - `context`: User-provided context pointer
pub type RuntimeStackCallback = unsafe extern "C" fn(
    emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
    context: *mut c_void,
);

/// Runtime stack representation for JSON serialization
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeStack {
    /// Format identifier for this runtime stack
    pub format: String,
    /// Array of runtime-specific stack frames
    pub frames: Vec<RuntimeFrame>,
    /// Runtime type identifier ("ruby", "python", "php", etc.)
    pub runtime_type: String,
}

/// JSON-serializable runtime stack frame
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeFrame {
    /// Function/method name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Source file name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number in source file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number in source file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Class name for OOP languages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    /// Module/namespace name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_name: Option<String>,
}

/// Errors that can occur during callback registration
#[derive(Debug, Error)]
pub enum CallbackError {
    #[error("A runtime callback is already registered")]
    AlreadyRegistered,
    #[error("Null callback function provided")]
    NullCallback,
}

/// Global storage for the runtime callback
///
/// Uses atomic pointer to ensure safe access from signal handlers
/// The pointer references a boxed tuple of (callback_fn, context)
static RUNTIME_CALLBACK: AtomicPtr<(RuntimeStackCallback, *mut c_void)> =
    AtomicPtr::new(ptr::null_mut());

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
/// - `Ok(())` if registration succeeds
/// - `Err(CallbackError)` if registration fails
///
/// # Safety
/// - The callback must be signal-safe
/// - The context pointer must remain valid for the lifetime of the process
/// - Only one callback can be registered at a time
///
/// # Example
/// ```c
/// unsafe extern "C" fn my_runtime_callback(
///     emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
///     context: *mut c_void
/// ) {
///     // Collect runtime frames...
/// }
///
/// register_runtime_stack_callback(my_runtime_callback, ptr::null_mut());
/// ```
pub fn register_runtime_stack_callback(
    callback: RuntimeStackCallback,
    context: *mut c_void,
) -> Result<(), CallbackError> {
    // Create the callback data on the heap
    let callback_data = Box::into_raw(Box::new((callback, context)));

    // Attempt to store it atomically
    let previous = RUNTIME_CALLBACK.compare_exchange(
        ptr::null_mut(),
        callback_data,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );

    match previous {
        Ok(_) => Ok(()),
        Err(_) => {
            // Clean up the allocation since we couldn't store it
            let _ = unsafe { Box::from_raw(callback_data) };
            Err(CallbackError::AlreadyRegistered)
        }
    }
}

/// Internal function to invoke the registered runtime callback with direct pipe writing
///
/// This is called during crash handling to collect runtime-specific stack frames
/// and write them directly to the provided writer for efficiency.
///
/// # Safety
/// This function is intended to be called from signal handlers and must maintain
/// signal safety. It does not perform any dynamic allocation.
pub(crate) unsafe fn invoke_runtime_callback_with_writer<W: std::io::Write>(
    writer: &mut W,
) -> Result<usize, std::io::Error> {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return Ok(0);
    }

    let (callback_fn, context) = &*callback_ptr;

    // Reset frame counter
    FRAME_COUNT.with(|count| count.set(0));

    // Define the emit_frame function that writes directly to the pipe
    unsafe extern "C" fn emit_frame_collector(frame: *const RuntimeStackFrame) {
        // We need access to the writer, so we'll use thread-local storage for it
        FRAME_WRITER.with(|writer_cell| {
            if let Some(writer_ptr) = *writer_cell.borrow() {
                let writer = &mut *writer_ptr;
                FRAME_COUNT.with(|count| {
                    let current_count = count.get();

                    // Add comma separator for frames after the first
                    if current_count > 0 {
                        let _ = write!(writer, ", ");
                    }

                    // Write the frame as JSON
                    let _ = emit_frame_as_json(writer, frame);
                    let _ = writer.flush();

                    count.set(current_count + 1);
                });
            }
        });
    }

    // Store the writer in thread-local storage so emit_frame_collector can access it
    FRAME_WRITER.with(|writer_cell| {
        *writer_cell.borrow_mut() = Some(writer as *mut W as *mut dyn std::io::Write);
    });

    // Invoke the user callback
    callback_fn(emit_frame_collector, *context);

    // Clear the writer reference
    FRAME_WRITER.with(|writer_cell| {
        *writer_cell.borrow_mut() = None;
    });

    // Return the number of frames written
    Ok(FRAME_COUNT.with(|count| count.get()))
}

thread_local! {
    static FRAME_WRITER: std::cell::RefCell<Option<*mut dyn std::io::Write>> = const { std::cell::RefCell::new(None) };
    static FRAME_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Emit a single runtime frame as JSON to the writer
///
/// This function writes a RuntimeStackFrame directly as JSON without intermediate allocation.
/// It must be signal-safe.
unsafe fn emit_frame_as_json(
    writer: &mut dyn std::io::Write,
    frame: *const RuntimeStackFrame,
) -> std::io::Result<()> {
    if frame.is_null() {
        return Ok(());
    }

    let frame_ref = &*frame;
    write!(writer, "{{")?;
    let mut first = true;

    // Convert C strings to Rust strings and write JSON fields
    if !frame_ref.function_name.is_null() {
        let c_str = std::ffi::CStr::from_ptr(frame_ref.function_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                write!(writer, "\"function\": \"{}\"", s)?;
                first = false;
            }
        }
    }

    if !frame_ref.file_name.is_null() {
        let c_str = std::ffi::CStr::from_ptr(frame_ref.file_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                if !first {
                    write!(writer, ", ")?;
                }
                write!(writer, "\"file\": \"{}\"", s)?;
                first = false;
            }
        }
    }

    if frame_ref.line_number != 0 {
        if !first {
            write!(writer, ", ")?;
        }
        write!(writer, "\"line\": {}", frame_ref.line_number)?;
        first = false;
    }

    if frame_ref.column_number != 0 {
        if !first {
            write!(writer, ", ")?;
        }
        write!(writer, "\"column\": {}", frame_ref.column_number)?;
        first = false;
    }

    if !frame_ref.class_name.is_null() {
        let c_str = std::ffi::CStr::from_ptr(frame_ref.class_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                if !first {
                    write!(writer, ", ")?;
                }
                write!(writer, "\"class_name\": \"{}\"", s)?;
                first = false;
            }
        }
    }

    if !frame_ref.module_name.is_null() {
        let c_str = std::ffi::CStr::from_ptr(frame_ref.module_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                if !first {
                    write!(writer, ", ")?;
                }
                write!(writer, "\"module_name\": \"{}\"", s)?;
            }
        }
    }

    write!(writer, "}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::Mutex;

    // Use a mutex to ensure tests run sequentially to avoid race conditions
    // with the global static variable
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn test_callback(
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

    fn ensure_callback_cleared() {
        // Ensure no callback is registered before starting
        let old_ptr = RUNTIME_CALLBACK.swap(ptr::null_mut(), Ordering::SeqCst);
        if !old_ptr.is_null() {
            let _ = unsafe { Box::from_raw(old_ptr) };
        }
    }

    #[test]
    fn test_callback_registration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Test successful registration
        let result = register_runtime_stack_callback(test_callback, ptr::null_mut());
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Test duplicate registration fails
        let result = register_runtime_stack_callback(test_callback, ptr::null_mut());
        assert!(matches!(result, Err(CallbackError::AlreadyRegistered)));

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    fn test_frame_collection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result = register_runtime_stack_callback(test_callback, ptr::null_mut());
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Invoke callback and collect frames using writer
        let mut buffer = Vec::new();
        let frame_count = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(frame_count.is_ok(), "Failed to invoke callback with writer");

        let frame_count = frame_count.unwrap();
        assert_eq!(frame_count, 1, "Expected 1 frame to be written");

        // Convert buffer to string and check JSON format
        let json_output = String::from_utf8(buffer).expect("Invalid UTF-8 in output");

        // Should contain the frame data as JSON
        assert!(
            json_output.contains("\"function\""),
            "Missing function field"
        );
        assert!(
            json_output.contains("test_function"),
            "Missing function name"
        );
        assert!(json_output.contains("\"file\""), "Missing file field");
        assert!(json_output.contains("test.rb"), "Missing file name");
        assert!(json_output.contains("\"line\": 42"), "Missing line number");
        assert!(
            json_output.contains("\"column\": 10"),
            "Missing column number"
        );
        assert!(
            json_output.contains("\"class_name\""),
            "Missing class_name field"
        );
        assert!(json_output.contains("TestClass"), "Missing class name");

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    fn test_no_callback_registered() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Test that invoke_runtime_callback_with_writer returns 0 frames when no callback is registered
        let mut buffer = Vec::new();
        let frame_count = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(frame_count.is_ok(), "Failed to invoke callback with writer");

        let frame_count = frame_count.unwrap();
        assert_eq!(
            frame_count, 0,
            "Expected 0 frames when no callback registered"
        );

        // Buffer should be empty
        assert!(
            buffer.is_empty(),
            "Expected empty buffer when no callback registered"
        );
    }

    #[test]
    fn test_direct_pipe_writing() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result = register_runtime_stack_callback(test_callback, ptr::null_mut());
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Test writing directly to a buffer
        let mut buffer = Vec::new();
        let frame_count = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(frame_count.is_ok(), "Failed to invoke callback with writer");

        let frame_count = frame_count.unwrap();
        assert_eq!(frame_count, 1, "Expected 1 frame to be written");

        // Convert buffer to string and check JSON format
        let json_output = String::from_utf8(buffer).expect("Invalid UTF-8 in output");

        // Should contain the frame data as JSON
        assert!(
            json_output.contains("\"function\""),
            "Missing function field"
        );
        assert!(
            json_output.contains("test_function"),
            "Missing function name"
        );
        assert!(json_output.contains("\"file\""), "Missing file field");
        assert!(json_output.contains("test.rb"), "Missing file name");
        assert!(json_output.contains("\"line\": 42"), "Missing line number");
        assert!(
            json_output.contains("\"column\": 10"),
            "Missing column number"
        );
        assert!(
            json_output.contains("\"class_name\""),
            "Missing class_name field"
        );
        assert!(json_output.contains("TestClass"), "Missing class name");

        // Clean up
        ensure_callback_cleared();
    }
}
