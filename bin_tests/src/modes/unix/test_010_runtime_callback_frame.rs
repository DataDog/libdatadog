// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using frame-by-frame mode.
// It registers a test callback that provides mock runtime stack frames,
// then crashes and verifies that the runtime frames appear in the crash report.
//
// This test uses frame-by-frame callback to emit structured runtime stack data.

use crate::modes::behavior::Behavior;
use datadog_crashtracker::{
    clear_runtime_callback, register_runtime_frame_callback, CrashtrackerConfiguration,
    RuntimeStackFrame,
};
use ddcommon_ffi::CharSlice;
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
        register_runtime_frame_callback(test_runtime_callback_frame)
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
) {
    static FUNCTION_NAME_1: &str = "test_module.TestClass.runtime_function_1";
    static FUNCTION_NAME_2: &str = "my_package.submodule.MyModule.runtime_function_2";
    static FUNCTION_NAME_3: &str = "__main__.runtime_main";
    static FILE_NAME_1: &str = "script.py";
    static FILE_NAME_2: &str = "module.py";
    static FILE_NAME_3: &str = "main.py";

    let frame1 = RuntimeStackFrame {
        function_name: CharSlice::from(FUNCTION_NAME_1),
        file_name: CharSlice::from(FILE_NAME_1),
        line_number: 42,
        column_number: 15,
    };
    emit_frame(&frame1);

    let frame2 = RuntimeStackFrame {
        function_name: CharSlice::from(FUNCTION_NAME_2),
        file_name: CharSlice::from(FILE_NAME_2),
        line_number: 100,
        column_number: 8,
    };
    emit_frame(&frame2);

    let frame3 = RuntimeStackFrame {
        function_name: CharSlice::from(FUNCTION_NAME_3),
        file_name: CharSlice::from(FILE_NAME_3),
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

        assert!(!is_runtime_callback_registered());

        let result = register_runtime_frame_callback(test_runtime_callback_frame);
        assert!(result.is_ok(), "Frame callback registration should succeed");
        assert!(
            is_runtime_callback_registered(),
            "Callback should be registered"
        );

        unsafe {
            clear_runtime_callback();
        }
        assert!(
            !is_runtime_callback_registered(),
            "Callback should be cleared"
        );
    }
}
