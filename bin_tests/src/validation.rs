// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Validation helpers for crash tracking tests.
//! This module provides reusable validation functions and a fluent API for asserting
//! crash report properties.

use anyhow::{Context, Result};
use serde_json::Value;
use std::{fs, path::Path};

/// Validates that the standard output files (stdout/stderr) contain expected content.
pub fn validate_std_outputs(output_dir: &Path) -> Result<()> {
    let stderr_path = output_dir.join("out.stderr");
    let stderr = fs::read(&stderr_path)
        .with_context(|| format!("reading crashtracker stderr at {:?}", stderr_path))?;

    let stdout_path = output_dir.join("out.stdout");
    let stdout = fs::read(&stdout_path)
        .with_context(|| format!("reading crashtracker stdout at {:?}", stdout_path))?;

    let s = String::from_utf8(stderr);
    anyhow::ensure!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "Unexpected stderr: {:?}", s
    );

    anyhow::ensure!(
        String::from_utf8(stdout).as_deref() == Ok(""),
        "Expected empty stdout"
    );

    Ok(())
}

/// Validates that the crash payload contains the expected standard counters.
pub fn validate_standard_counters(payload: &Value) -> Result<()> {
    let expected = serde_json::json!({
        "profiler_collecting_sample": 1,
        "profiler_inactive": 0,
        "profiler_serializing": 0,
        "profiler_unwinding": 0
    });

    anyhow::ensure!(
        payload["counters"] == expected,
        "Counters mismatch. Expected: {:?}, Got: {:?}",
        expected,
        payload["counters"]
    );

    Ok(())
}

/// Reads and parses a crash payload from a file.
pub fn read_and_parse_crash_payload(path: &Path) -> Result<Value> {
    let crash_profile = fs::read(path)
        .with_context(|| format!("reading crashtracker profiling payload at {:?}", path))?;

    serde_json::from_slice::<Value>(&crash_profile)
        .with_context(|| "deserializing crashtracker profiling payload to json")
}

/// A fluent API for validating crash payloads.
/// Allows chaining multiple validations together.
///
/// # Example
/// ```ignore
/// PayloadValidator::new(&crash_payload)
///     .validate_counters()?
///     .validate_experimental_section_exists()?
///     .validate_runtime_stack_format("Datadog Runtime Callback 1.0")?;
/// ```
pub struct PayloadValidator<'a> {
    payload: &'a Value,
}

impl<'a> PayloadValidator<'a> {
    /// Creates a new payload validator.
    pub fn new(payload: &'a Value) -> Self {
        Self { payload }
    }

    /// Validates the standard counters in the payload.
    pub fn validate_counters(self) -> Result<Self> {
        validate_standard_counters(self.payload)?;
        Ok(self)
    }

    /// Validates that the experimental section exists in the payload.
    pub fn validate_experimental_section_exists(self) -> Result<Self> {
        anyhow::ensure!(
            self.payload.get("experimental").is_some(),
            "Experimental section should be present in crash payload"
        );
        Ok(self)
    }

    /// Validates that the experimental section does NOT exist in the payload.
    pub fn validate_no_experimental_section(self) -> Result<Self> {
        anyhow::ensure!(
            self.payload.get("experimental").is_none(),
            "Experimental section should NOT be present in crash payload"
        );
        Ok(self)
    }

    /// Validates that runtime_stack exists in the experimental section.
    pub fn validate_runtime_stack_exists(self) -> Result<Self> {
        let experimental = self
            .payload
            .get("experimental")
            .context("Experimental section should be present")?;

        anyhow::ensure!(
            experimental.get("runtime_stack").is_some(),
            "Runtime stack should be present in experimental section"
        );
        Ok(self)
    }

    /// Validates that runtime_stack does NOT exist in the experimental section.
    pub fn validate_no_runtime_stack(self) -> Result<Self> {
        if let Some(experimental) = self.payload.get("experimental") {
            anyhow::ensure!(
                experimental.get("runtime_stack").is_none(),
                "Runtime stack should NOT be present in experimental section when no callback is registered. Got: {:?}",
                experimental.get("runtime_stack")
            );
        }
        Ok(self)
    }

    /// Validates the runtime stack format string.
    pub fn validate_runtime_stack_format(self, expected_format: &str) -> Result<Self> {
        let runtime_stack = self.payload["experimental"]["runtime_stack"]
            .as_object()
            .context("Runtime stack should be an object")?;

        let format = runtime_stack["format"]
            .as_str()
            .context("Format should be a string")?;

        anyhow::ensure!(
            format == expected_format,
            "Expected format '{}', got '{}'",
            expected_format,
            format
        );

        Ok(self)
    }

    /// Validates that the runtime stack has frames.
    pub fn validate_runtime_stack_has_frames(self, expected_count: usize) -> Result<Self> {
        let frames = self.payload["experimental"]["runtime_stack"]["frames"]
            .as_array()
            .context("Runtime stack frames should be an array")?;

        anyhow::ensure!(
            frames.len() == expected_count,
            "Expected {} runtime frames, got {}",
            expected_count,
            frames.len()
        );

        Ok(self)
    }

    /// Validates that the runtime stack has a stacktrace_string field.
    pub fn validate_runtime_stack_has_string(self) -> Result<Self> {
        let stacktrace_string = self.payload["experimental"]["runtime_stack"]
            .get("stacktrace_string")
            .context("Runtime stack should have stacktrace_string field")?;

        anyhow::ensure!(
            stacktrace_string.is_string(),
            "Runtime stacktrace_string should be a string"
        );

        Ok(self)
    }

    /// Validates the error message contains specific text.
    pub fn validate_error_message_contains(self, expected_substring: &str) -> Result<Self> {
        let message = self.payload["error"]["message"]
            .as_str()
            .context("Error message should be a string")?;

        anyhow::ensure!(
            message.contains(expected_substring),
            "Expected error message to contain '{}', got: '{}'",
            expected_substring,
            message
        );

        Ok(self)
    }

    /// Validates the error kind.
    pub fn validate_error_kind(self, expected_kind: &str) -> Result<Self> {
        let kind = self.payload["error"]["kind"]
            .as_str()
            .context("Error kind should be a string")?;

        anyhow::ensure!(
            kind == expected_kind,
            "Expected error kind '{}', got '{}'",
            expected_kind,
            kind
        );

        Ok(self)
    }

    /// Validates that the callstack contains the expected functions in order.
    /// This is useful for tests that verify specific call chains are preserved.
    pub fn validate_callstack_functions(self, expected_functions: &[&str]) -> Result<Self> {
        let crashing_callstack = &self.payload["error"]["stack"]["frames"];
        let frames = crashing_callstack
            .as_array()
            .context("error.stack.frames should be an array")?;

        anyhow::ensure!(
            frames.len() >= expected_functions.len(),
            "Crashing thread callstack has fewer frames than expected. Current: {}, Expected: {}",
            frames.len(),
            expected_functions.len()
        );

        let function_names: Vec<&str> = frames
            .iter()
            .filter_map(|f| f["function"].as_str())
            .collect();

        for (i, expected) in expected_functions.iter().enumerate() {
            let actual = function_names.get(i).copied().unwrap_or("");
            anyhow::ensure!(
                actual == *expected,
                "Callstack mismatch at position {}: expected '{}', got '{}'",
                i,
                expected,
                actual
            );
        }

        Ok(self)
    }

    /// Returns the underlying payload reference.
    pub fn payload(&self) -> &'a Value {
        self.payload
    }

    /// Consumes the validator and returns the payload reference.
    pub fn finish(self) -> &'a Value {
        self.payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validator_chaining() {
        let payload = json!({
            "counters": {
                "profiler_collecting_sample": 1,
                "profiler_inactive": 0,
                "profiler_serializing": 0,
                "profiler_unwinding": 0
            },
            "experimental": {
                "runtime_stack": {
                    "format": "Datadog Runtime Callback 1.0",
                    "frames": []
                }
            },
            "error": {
                "kind": "Panic",
                "message": "test panic message"
            }
        });

        let result = PayloadValidator::new(&payload)
            .validate_counters()
            .and_then(|v| v.validate_experimental_section_exists())
            .and_then(|v| v.validate_runtime_stack_exists())
            .and_then(|v| v.validate_runtime_stack_format("Datadog Runtime Callback 1.0"))
            .and_then(|v| v.validate_error_kind("Panic"))
            .and_then(|v| v.validate_error_message_contains("test panic"));

        assert!(result.is_ok());
    }

    #[test]
    fn test_validator_failure() {
        let payload = json!({
            "counters": {
                "profiler_collecting_sample": 0,  // Wrong value
                "profiler_inactive": 0,
                "profiler_serializing": 0,
                "profiler_unwinding": 0
            }
        });

        let result = PayloadValidator::new(&payload).validate_counters();
        assert!(result.is_err());
    }
}
