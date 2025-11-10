// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using frame-by-frame mode.
// It registers a test callback that provides mock runtime stack frames,
// then crashes and verifies that the runtime frames appear in the crash report.
//
// This test uses frame-by-frame callback to emit structured runtime stack data.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::{
    clear_runtime_callback, register_runtime_frame_callback, CrashtrackerConfiguration,
    RuntimeStackFrame,
};
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
    emit_frame: unsafe extern "C" fn(&RuntimeStackFrame),
) {
    static FUNCTION_NAME_1: &str = "runtime_function_1";
    static FUNCTION_NAME_2: &str = "runtime_function_2";
    static FUNCTION_NAME_3: &str = "runtime_main";
    static FILE_NAME_1: &str = "script.py";
    static FILE_NAME_2: &str = "module.py";
    static FILE_NAME_3: &str = "main.py";
    static TYPE_NAME_1: &str = "TestModule.TestClass";
    static TYPE_NAME_2: &str = "MyPackage.Submodule.MyModule";

    let frame1 = RuntimeStackFrame {
        type_name: TYPE_NAME_1.as_bytes(),
        function: FUNCTION_NAME_1.as_bytes(),
        file: FILE_NAME_1.as_bytes(),
        line: 42,
        column: 15,
    };
    emit_frame(&frame1);

    let frame2 = RuntimeStackFrame {
        type_name: TYPE_NAME_2.as_bytes(),
        function: FUNCTION_NAME_2.as_bytes(),
        file: FILE_NAME_2.as_bytes(),
        line: 100,
        column: 8,
    };
    emit_frame(&frame2);

    let frame3 = RuntimeStackFrame {
        type_name: b"", // Empty for null case
        function: FUNCTION_NAME_3.as_bytes(),
        file: FILE_NAME_3.as_bytes(),
        line: 10,
        column: 1,
    };
    emit_frame(&frame3);
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_crashtracker::{clear_runtime_callback, is_runtime_callback_registered};

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
