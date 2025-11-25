// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generic test runner infrastructure for crash tracking tests.
//! This module provides a configurable test runner that eliminates code duplication
//! across different test scenarios.

use crate::{
    test_types::{CrashType, TestMode},
    validation::{read_and_parse_crash_payload, validate_std_outputs, PayloadValidator},
    ArtifactType, ArtifactsBuild, BuildProfile,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process,
};

/// Type alias for validator functions used in test runners.
/// A validator takes a crash payload and test fixtures, returning a Result.
pub type ValidatorFn = Box<dyn Fn(&Value, &TestFixtures) -> Result<()>>;

/// Configuration for a crash tracking test.
#[derive(Debug, Clone)]
pub struct CrashTestConfig<'a> {
    /// Build profile for the test binaries
    pub profile: BuildProfile,
    /// Test mode (behavior)
    pub mode: TestMode,
    /// Type of crash to trigger
    pub crash_type: CrashType,
    /// Additional environment variables to set
    pub env_vars: Vec<(&'a str, &'a str)>,
    /// Custom artifacts to use (if None, uses standard crashtracker artifacts)
    pub custom_artifacts: Option<Vec<ArtifactsBuild>>,
}

impl<'a> CrashTestConfig<'a> {
    /// Creates a new test configuration with the given profile, mode, and crash type.
    pub fn new(profile: BuildProfile, mode: TestMode, crash_type: CrashType) -> Self {
        Self {
            profile,
            mode,
            crash_type,
            env_vars: vec![],
            custom_artifacts: None,
        }
    }

    /// Creates a test configuration from string arguments (for backward compatibility).
    pub fn from_strings(
        profile: BuildProfile,
        mode: &str,
        crash_type: &str,
    ) -> Result<Self, String> {
        let mode = mode.parse::<TestMode>()?;
        let crash_type = crash_type.parse::<CrashType>()?;
        Ok(Self::new(profile, mode, crash_type))
    }

    /// Adds an environment variable to the test configuration.
    pub fn with_env(mut self, key: &'a str, value: &'a str) -> Self {
        self.env_vars.push((key, value));
        self
    }

    /// Sets custom artifacts for the test.
    pub fn with_artifacts(mut self, artifacts: Vec<ArtifactsBuild>) -> Self {
        self.custom_artifacts = Some(artifacts);
        self
    }
}

/// Result of setting up test fixtures.
pub struct TestFixtures {
    pub crash_profile_path: PathBuf,
    pub crash_telemetry_path: PathBuf,
    pub output_dir: PathBuf,
    #[allow(dead_code)]
    tmpdir: tempfile::TempDir,
}

impl TestFixtures {
    pub fn new() -> Result<Self> {
        let tmpdir = tempfile::TempDir::new().context("Failed to create temporary directory")?;
        let dirpath = tmpdir.path();

        Ok(Self {
            crash_profile_path: extend_path(dirpath, "crash"),
            crash_telemetry_path: extend_path(dirpath, "crash.telemetry"),
            output_dir: dirpath.to_path_buf(),
            tmpdir,
        })
    }
}

fn extend_path(dir: &Path, file: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.push(file);
    path
}

/// Standard artifacts used in most crash tracking tests.
pub struct StandardArtifacts {
    pub crashtracker_bin: ArtifactsBuild,
    pub crashtracker_receiver: ArtifactsBuild,
}

impl StandardArtifacts {
    pub fn new(profile: BuildProfile) -> Self {
        Self {
            crashtracker_bin: ArtifactsBuild {
                name: "crashtracker_bin_test".to_owned(),
                build_profile: profile,
                artifact_type: ArtifactType::Bin,
                ..Default::default()
            },
            crashtracker_receiver: ArtifactsBuild {
                name: "test_crashtracker_receiver".to_owned(),
                build_profile: profile,
                artifact_type: ArtifactType::Bin,
                ..Default::default()
            },
        }
    }

    pub fn as_slice(&self) -> Vec<&ArtifactsBuild> {
        vec![&self.crashtracker_bin, &self.crashtracker_receiver]
    }
}

/// Generic crash test runner that handles common test logic.
///
/// This function:
/// 1. Sets up test fixtures and builds artifacts
/// 2. Spawns the test process with appropriate arguments
/// 3. Waits for the process to complete
/// 4. Validates standard outputs and crash payload
/// 5. Calls the provided validator for custom validation
///
/// # Arguments
/// * `config` - Test configuration
/// * `artifacts_map` - Map of artifacts to their paths
/// * `artifacts` - The standard artifacts to use
/// * `validator` - Custom validation function for the crash payload
pub fn run_crash_test_with_artifacts<F>(
    config: &CrashTestConfig,
    artifacts_map: &HashMap<&ArtifactsBuild, PathBuf>,
    artifacts: &StandardArtifacts,
    validator: F,
) -> Result<()>
where
    F: FnOnce(&Value, &TestFixtures) -> Result<()>,
{
    let fixtures = TestFixtures::new()?;

    let mut cmd = process::Command::new(&artifacts_map[&artifacts.crashtracker_bin]);
    cmd.arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(&artifacts_map[&artifacts.crashtracker_receiver])
        .arg(&fixtures.output_dir)
        .arg(config.mode.as_str())
        .arg(config.crash_type.as_str());

    for (key, val) in &config.env_vars {
        cmd.env(key, val);
    }

    let mut p = cmd.spawn().context("Failed to spawn test process")?;

    let exit_status = crate::timeit!("exit after signal", { p.wait()? });

    // Validate exit status
    assert_exit_status(exit_status, config.crash_type)?;

    // Validate standard outputs
    validate_std_outputs(&fixtures.output_dir)?;

    // Read and parse crash payload
    let crash_payload = read_and_parse_crash_payload(&fixtures.crash_profile_path)?;

    // Run custom validator
    validator(&crash_payload, &fixtures)?;

    Ok(())
}

/// Validates the process exit status matches expectations for the crash type.
fn assert_exit_status(exit_status: process::ExitStatus, crash_type: CrashType) -> Result<()> {
    let expected_success = crash_type.expects_success();
    let actual_success = exit_status.success();

    anyhow::ensure!(
        expected_success == actual_success,
        "Exit status mismatch for {:?}: expected success={}, got success={} (exit code: {:?})",
        crash_type,
        expected_success,
        actual_success,
        exit_status.code()
    );

    Ok(())
}

/// A builder for creating standardized payload validators.
pub struct ValidatorBuilder {
    validate_counters: bool,
    validate_siginfo: bool,
    validate_telemetry: bool,
    custom_validators: Vec<ValidatorFn>,
}

impl ValidatorBuilder {
    pub fn new() -> Self {
        Self {
            validate_counters: true,
            validate_siginfo: true,
            validate_telemetry: true,
            custom_validators: vec![],
        }
    }

    /// Skip standard counter validation.
    pub fn skip_counters(mut self) -> Self {
        self.validate_counters = false;
        self
    }

    /// Skip siginfo validation.
    pub fn skip_siginfo(mut self) -> Self {
        self.validate_siginfo = false;
        self
    }

    /// Skip telemetry validation.
    pub fn skip_telemetry(mut self) -> Self {
        self.validate_telemetry = false;
        self
    }

    /// Add a custom validator function.
    pub fn with_custom<F>(mut self, validator: F) -> Self
    where
        F: Fn(&Value, &TestFixtures) -> Result<()> + 'static,
    {
        self.custom_validators.push(Box::new(validator));
        self
    }

    /// Build the validator function.
    pub fn build(self) -> impl Fn(&Value, &TestFixtures) -> Result<()> {
        move |payload: &Value, fixtures: &TestFixtures| {
            if self.validate_counters {
                PayloadValidator::new(payload)
                    .validate_counters()
                    .context("Standard counter validation failed")?;
            }

            // Note: siginfo validation would need access to crash_type
            // We'll handle this in the higher-level runner

            for validator in &self.custom_validators {
                validator(payload, fixtures)?;
            }

            if self.validate_telemetry {
                // Telemetry validation would also need crash_type
                // We'll handle this in the higher-level runner
            }

            Ok(())
        }
    }
}

impl Default for ValidatorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config = CrashTestConfig::new(
            BuildProfile::Debug,
            TestMode::DoNothing,
            CrashType::NullDeref,
        );

        assert_eq!(config.profile, BuildProfile::Debug);
        assert_eq!(config.mode, TestMode::DoNothing);
        assert_eq!(config.crash_type, CrashType::NullDeref);
        assert!(config.env_vars.is_empty());
    }

    #[test]
    fn test_config_with_env() {
        let config = CrashTestConfig::new(
            BuildProfile::Release,
            TestMode::DoNothing,
            CrashType::NullDeref,
        )
        .with_env("TEST_VAR", "test_value");

        assert_eq!(config.env_vars.len(), 1);
        assert_eq!(config.env_vars[0], ("TEST_VAR", "test_value"));
    }

    #[test]
    fn test_config_from_strings() {
        let config =
            CrashTestConfig::from_strings(BuildProfile::Debug, "donothing", "null_deref").unwrap();

        assert_eq!(config.mode, TestMode::DoNothing);
        assert_eq!(config.crash_type, CrashType::NullDeref);
    }

    #[test]
    fn test_config_from_invalid_strings() {
        let result =
            CrashTestConfig::from_strings(BuildProfile::Debug, "invalid_mode", "null_deref");

        assert!(result.is_err());
    }

    #[test]
    fn test_crash_type_exit_status() {
        // These should expect failure
        assert!(!CrashType::NullDeref.expects_success());
        assert!(!CrashType::KillSigAbrt.expects_success());
        assert!(!CrashType::RaiseSigAbrt.expects_success());

        // These should expect success
        assert!(CrashType::KillSigBus.expects_success());
        assert!(CrashType::KillSigSegv.expects_success());
    }
}
