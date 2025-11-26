// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows crash tracking test binary.
//! This binary initializes Windows crash tracking and triggers various types of crashes.

#[cfg(not(windows))]
fn main() {
    eprintln!("This binary is Windows-only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows::main()
}

#[cfg(windows)]
mod windows {
    use anyhow::Context;
    use libdd_common::Endpoint;
    use libdd_crashtracker::{init_crashtracking_windows, Metadata};
    use std::env;
    use std::path::Path;

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Missing output URL argument")?;
        let output_dir = args.next().context("Missing output directory argument")?;
        let mode_str = args.next().context("Missing mode argument")?;
        let crash_type = args.next().context("Missing crash type argument")?;
        anyhow::ensure!(args.next().is_none(), "Unexpected extra arguments");

        let output_dir: &Path = output_dir.as_ref();

        // Get WER module path from environment
        let wer_module_path =
            env::var("WER_MODULE_PATH").context("WER_MODULE_PATH environment variable not set")?;

        let endpoint = if output_url.is_empty() || output_url.starts_with("file://") {
            // For file:// URLs or empty, we'll write to a file instead
            None
        } else {
            Some(Endpoint::from_slice(&output_url))
        };

        let metadata = Metadata {
            library_name: "libdatadog".to_owned(),
            library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                "service:foo".to_string(),
                "service_version:bar".to_string(),
                "runtime-id:xyz".to_string(),
                "language:native".to_string(),
                "env:test".to_string(),
            ],
        };

        // Apply test mode behavior (before init)
        apply_test_mode_pre_init(&mode_str, output_dir)?;

        // Initialize Windows crash tracking
        init_crashtracking_windows(wer_module_path, endpoint.as_ref(), metadata)
            .context("Failed to initialize Windows crash tracking")?;

        // Apply test mode behavior (after init)
        apply_test_mode_post_init(&mode_str, output_dir)?;

        // Trigger the crash
        trigger_crash(&crash_type)?;

        // Should not reach here (process should have crashed)
        anyhow::bail!("Process did not crash as expected")
    }

    /// Apply test mode behaviors before initialization.
    fn apply_test_mode_pre_init(mode: &str, _output_dir: &Path) -> anyhow::Result<()> {
        match mode {
            "basic" => {
                // No special setup
                Ok(())
            }
            "multithreaded" => {
                // Will spawn threads after init
                Ok(())
            }
            "deepstack" => {
                // Will create deep stack during crash
                Ok(())
            }
            "registry" => {
                // Registry testing - no special pre-init
                Ok(())
            }
            "custom_wer" => {
                // Custom WER settings - not implemented yet
                Ok(())
            }
            "wer_context" => {
                // WER context testing - no special pre-init
                Ok(())
            }
            _ => anyhow::bail!("Unknown test mode: {}", mode),
        }
    }

    /// Apply test mode behaviors after initialization.
    fn apply_test_mode_post_init(mode: &str, _output_dir: &Path) -> anyhow::Result<()> {
        match mode {
            "basic" | "registry" | "wer_context" => {
                // No special post-init behavior
                Ok(())
            }
            "multithreaded" => {
                // Multithreading test - we'll crash in the main thread for now
                // Future: spawn threads and crash in one of them
                Ok(())
            }
            "deepstack" => {
                // Deep stack will be created during crash trigger
                Ok(())
            }
            "custom_wer" => {
                // Not implemented yet
                Ok(())
            }
            _ => anyhow::bail!("Unknown test mode: {}", mode),
        }
    }

    /// Triggers various types of crashes.
    fn trigger_crash(crash_type: &str) -> anyhow::Result<()> {
        match crash_type {
            "access_violation_null" => {
                // Null pointer dereference
                unsafe {
                    let ptr: *const i32 = std::ptr::null();
                    let _value = *ptr; // Should cause EXCEPTION_ACCESS_VIOLATION
                }
            }
            "access_violation_read" => {
                // Read from invalid address
                unsafe {
                    let ptr: *const i32 = 0xDEADBEEF as *const i32;
                    let _value = *ptr; // Should cause EXCEPTION_ACCESS_VIOLATION
                }
            }
            "access_violation_write" => {
                // Write to invalid address
                unsafe {
                    let ptr: *mut i32 = 0xDEADBEEF as *mut i32;
                    *ptr = 42; // Should cause EXCEPTION_ACCESS_VIOLATION
                }
            }
            "divide_by_zero" => {
                // Integer division by zero
                // Use black_box to prevent compile-time evaluation
                let x = std::hint::black_box(1);
                let y = std::hint::black_box(0);
                #[allow(clippy::let_unit_value)]
                let _result = x / y; // Should cause EXCEPTION_INT_DIVIDE_BY_ZERO
            }
            "stack_overflow" => {
                // Infinite recursion to cause stack overflow
                #[allow(unconditional_recursion)]
                fn recurse() {
                    let _large_array = [0u8; 10240]; // 10KB per frame
                    recurse();
                }
                recurse(); // Should cause EXCEPTION_STACK_OVERFLOW
            }
            "illegal_instruction" => {
                // Execute illegal instruction
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    std::arch::asm!("ud2"); // Undefined instruction
                }
                #[cfg(target_arch = "x86")]
                unsafe {
                    std::arch::asm!("ud2"); // Undefined instruction
                }
                #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
                {
                    // For ARM or other architectures, use abort as fallback
                    std::process::abort();
                }
            }
            "abort" => {
                // Explicit abort
                std::process::abort(); // Generates STATUS_STACK_BUFFER_OVERRUN or similar
            }
            _ => {
                anyhow::bail!("Unknown crash type: {}", crash_type);
            }
        }

        // Should never reach here
        Ok(())
    }
}
