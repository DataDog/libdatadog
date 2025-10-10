// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using frame-by-frame mode.
// It registers a test callback that provides mock runtime stack frames,
// then crashes and verifies that the runtime frames appear in the crash report.
//
// This test uses CallbackType::Frame to emit structured runtime stack data.

use crate::modes::behavior::Behavior;
use datadog_crashtracker::{
    clear_runtime_callback, register_runtime_stack_callback, CallbackType,
    CrashtrackerConfiguration, RuntimeStackFrame,
};
use std::ffi::c_char;
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Ensure clean state
        unsafe {
            clear_runtime_callback();
        }
        register_runtime_stack_callback(test_runtime_callback_frame, CallbackType::Frame)
            .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
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
    // Using fully qualified function names that include module/class hierarchy
    static FUNCTION_NAME_1: &[u8] = b"test_module.TestClass.runtime_function_1\0";
    static FUNCTION_NAME_2: &[u8] = b"my_package.submodule.MyModule.runtime_function_2\0";
    static FUNCTION_NAME_3: &[u8] = b"__main__.runtime_main\0";
    static FILE_NAME_1: &[u8] = b"script.py\0";
    static FILE_NAME_2: &[u8] = b"module.py\0";
    static FILE_NAME_3: &[u8] = b"main.py\0";

    // Frame 1: test_module.TestClass.runtime_function_1 in script.py
    let frame1 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_1.as_ptr() as *const c_char,
        file_name: FILE_NAME_1.as_ptr() as *const c_char,
        line_number: 42,
        column_number: 15,
    };
    emit_frame(&frame1);

    // Frame 2: my_package.submodule.MyModule.runtime_function_2 in module.py
    let frame2 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_2.as_ptr() as *const c_char,
        file_name: FILE_NAME_2.as_ptr() as *const c_char,
        line_number: 100,
        column_number: 8,
    };
    emit_frame(&frame2);

    // Frame 3: __main__.runtime_main in main.py
    let frame3 = RuntimeStackFrame {
        function_name: FUNCTION_NAME_3.as_ptr() as *const c_char,
        file_name: FILE_NAME_3.as_ptr() as *const c_char,
        line_number: 10,
        column_number: 1,
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
