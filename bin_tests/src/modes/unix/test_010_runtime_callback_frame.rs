// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using frame-by-frame mode.
// It registers a test callback that provides mock runtime stack frames,
// then crashes and verifies that the runtime frames appear in the crash report.
//
// This test uses CallbackType::Frame to emit structured runtime stack data.

use crate::modes::behavior::{file_write_msg, Behavior};
use datadog_crashtracker::{
    register_runtime_stack_callback, CallbackType, CrashtrackerConfiguration, RuntimeStackFrame,
};
use std::ffi::c_char;
use std::path::Path;
use std::ptr;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        // Write a marker file to indicate we're testing runtime callback frame mode
        let marker_file = output_dir.join("runtime_callback_test");
        file_write_msg(&marker_file, "frame_mode")?;
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Register our test runtime callback before crashtracker initialization
        register_runtime_stack_callback(test_runtime_callback_frame, CallbackType::Frame)
            .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Nothing to do post-initialization for this test
        Ok(())
    }
}

// Signal-safe test callback that emits mock runtime stack frames
unsafe extern "C" fn test_runtime_callback_frame(
    emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
    _emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
) {
    // Use static null-terminated strings to avoid allocation in signal context
    // In a real runtime, these would come from the runtime's managed string pool
    static FUNCTION_NAME_1: &[u8] = b"runtime_function_1\0";
    static FUNCTION_NAME_2: &[u8] = b"runtime_function_2\0";
    static FUNCTION_NAME_3: &[u8] = b"runtime_main\0";
    static FILE_NAME_1: &[u8] = b"script.py\0";
    static FILE_NAME_2: &[u8] = b"module.py\0";
    static FILE_NAME_3: &[u8] = b"main.py\0";
    static CLASS_NAME_1: &[u8] = b"TestClass\0";
    static CLASS_NAME_2: &[u8] = b"MyModule\0";
    static MODULE_NAME_1: &[u8] = b"test_module\0";
    static MODULE_NAME_2: &[u8] = b"my_package.submodule\0";
    static MODULE_NAME_3: &[u8] = b"__main__\0";

    // Frame 1: runtime_function_1 in script.py
    let frame1 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_1.as_ptr() as *const c_char,
        file_name: FILE_NAME_1.as_ptr() as *const c_char,
        line_number: 42,
        column_number: 15,
        class_name: CLASS_NAME_1.as_ptr() as *const c_char,
        module_name: MODULE_NAME_1.as_ptr() as *const c_char,
    };
    emit_frame(&frame1);

    // Frame 2: runtime_function_2 in module.py
    let frame2 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_2.as_ptr() as *const c_char,
        file_name: FILE_NAME_2.as_ptr() as *const c_char,
        line_number: 100,
        column_number: 8,
        class_name: CLASS_NAME_2.as_ptr() as *const c_char,
        module_name: MODULE_NAME_2.as_ptr() as *const c_char,
    };
    emit_frame(&frame2);

    // Frame 3: runtime_main in main.py (no class)
    let frame3 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_3.as_ptr() as *const c_char,
        file_name: FILE_NAME_3.as_ptr() as *const c_char,
        line_number: 10,
        column_number: 1,
        class_name: ptr::null(), // No class for main function
        module_name: MODULE_NAME_3.as_ptr() as *const c_char,
    };
    emit_frame(&frame3);
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_crashtracker::{clear_runtime_callback, is_runtime_callback_registered};

    #[test]
    fn test_runtime_callback_frame_registration() {
        // Ensure clean state
        unsafe {
            clear_runtime_callback();
        }

        // Test that no callback is initially registered
        assert!(!is_runtime_callback_registered());

        // Test frame mode registration
        let result =
            register_runtime_stack_callback(test_runtime_callback_frame, CallbackType::Frame);
        assert!(result.is_ok(), "Frame callback registration should succeed");
        assert!(
            is_runtime_callback_registered(),
            "Callback should be registered"
        );

        // Clean up
        unsafe {
            clear_runtime_callback();
        }
        assert!(
            !is_runtime_callback_registered(),
            "Callback should be cleared"
        );
    }
}
