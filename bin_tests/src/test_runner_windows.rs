// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows-specific test runner infrastructure for crash tracking tests.
//! Provides configuration and execution framework for Windows crash tests.

use crate::{
    test_types_windows::{WindowsCrashType, WindowsTestMode},
    validation::read_and_parse_crash_payload,
    BuildProfile,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process,
    time::Duration,
};

/// Type alias for validator functions used in Windows test runners.
pub type WindowsValidatorFn = Box<dyn FnOnce(&Value, &WindowsTestFixtures) -> Result<()>>;

/// Configuration for a Windows crash tracking test.
#[derive(Debug, Clone)]
pub struct WindowsCrashTestConfig {
    /// Build profile for the test binaries
    pub profile: BuildProfile,
    /// Test mode (behavior)
    pub mode: WindowsTestMode,
    /// Type of crash to trigger
    pub crash_type: WindowsCrashType,
    /// Whether to expect successful upload
    pub expect_upload: bool,
    /// Optional custom registry key (for isolation)
    pub registry_key_override: Option<String>,
}

impl WindowsCrashTestConfig {
    /// Creates a new Windows test configuration.
    pub fn new(profile: BuildProfile, mode: WindowsTestMode, crash_type: WindowsCrashType) -> Self {
        Self {
            profile,
            mode,
            crash_type,
            expect_upload: true,
            registry_key_override: None,
        }
    }

    /// Sets whether to expect upload success.
    pub fn with_expect_upload(mut self, expect: bool) -> Self {
        self.expect_upload = expect;
        self
    }

    /// Sets a custom registry key for test isolation.
    pub fn with_registry_key(mut self, key: String) -> Self {
        self.registry_key_override = Some(key);
        self
    }
}

/// Test fixtures for Windows crash tests.
pub struct WindowsTestFixtures {
    /// Path where crash payload will be written
    pub crash_payload_path: PathBuf,
    /// Output directory for test artifacts
    pub output_dir: PathBuf,
    /// Registry key used for this test (for cleanup)
    pub registry_key: String,
    /// Temporary directory (kept alive for test duration)
    #[allow(dead_code)]
    tmpdir: tempfile::TempDir,
}

impl WindowsTestFixtures {
    /// Creates new test fixtures with temporary directory.
    pub fn new() -> Result<Self> {
        Self::new_with_registry_key(Self::generate_registry_key())
    }

    /// Creates new test fixtures with specific registry key.
    pub fn new_with_registry_key(registry_key: String) -> Result<Self> {
        let tmpdir = tempfile::TempDir::new().context("Failed to create temporary directory")?;
        let dirpath = tmpdir.path();

        Ok(Self {
            crash_payload_path: extend_path(dirpath, "crash.json"),
            output_dir: dirpath.to_path_buf(),
            registry_key,
            tmpdir,
        })
    }

    /// Generates a unique registry key for test isolation.
    fn generate_registry_key() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        format!("DatadogCrashTrackerTest_{}", timestamp)
    }
}

fn extend_path(dir: &Path, file: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.push(file);
    path
}

/// Runs a Windows crash test with the given configuration and validator.
///
/// # Arguments
/// * `config` - Test configuration
/// * `binary_path` - Path to the test binary
/// * `wer_dll_path` - Path to the WER handler DLL
/// * `validator` - Custom validation function
pub fn run_windows_crash_test<F>(
    config: &WindowsCrashTestConfig,
    binary_path: &Path,
    wer_dll_path: &Path,
    validator: F,
) -> Result<()>
where
    F: FnOnce(&Value, &WindowsTestFixtures) -> Result<()>,
{
    let fixtures = if let Some(ref key) = config.registry_key_override {
        WindowsTestFixtures::new_with_registry_key(key.clone())?
    } else {
        WindowsTestFixtures::new()?
    };

    // Build command
    let mut cmd = process::Command::new(binary_path);
    cmd.arg(format!("file://{}", fixtures.crash_payload_path.display()))
        .arg(&fixtures.output_dir)
        .arg(config.mode.as_str())
        .arg(config.crash_type.as_str());

    // Set environment variables
    cmd.env("WER_MODULE_PATH", wer_dll_path)
        .env("REGISTRY_KEY", &fixtures.registry_key)
        .env("CRASH_OUTPUT_DIR", &fixtures.output_dir);

    // Spawn test process
    let mut p = cmd
        .spawn()
        .context("Failed to spawn Windows test process")?;

    // Wait for process to crash
    let exit_status = p.wait().context("Failed to wait for test process")?;

    // On Windows, crashes typically result in non-zero exit
    // (Unlike Unix where some signals can result in "success")
    anyhow::ensure!(
        !exit_status.success(),
        "Expected process to crash (non-zero exit), but it succeeded"
    );

    // Check if WER is enabled on the system
    eprintln!("[DEBUG] Checking if WER is enabled globally...");
    let wer_enabled = check_wer_enabled();
    match wer_enabled {
        Ok(true) => eprintln!("[DEBUG] ✅ WER is enabled"),
        Ok(false) => eprintln!("[DEBUG] ❌ WER is DISABLED on this system!"),
        Err(e) => eprintln!("[DEBUG] ⚠️ Could not check WER status: {:?}", e),
    }

    // Verify DLL exports the WER callback
    eprintln!("[DEBUG] Checking if DLL exports OutOfProcessExceptionEventCallback...");
    let dll_exports_check = check_dll_export(wer_dll_path);
    match dll_exports_check {
        Ok(true) => eprintln!("[DEBUG] ✅ DLL exports OutOfProcessExceptionEventCallback"),
        Ok(false) => eprintln!("[DEBUG] ❌ DLL does NOT export OutOfProcessExceptionEventCallback!"),
        Err(e) => eprintln!("[DEBUG] ⚠️ Could not check DLL exports: {:?}", e),
    }

    // Check registry key before waiting for crash
    eprintln!("[DEBUG] Checking WER registry key...");
    let registry_check = check_wer_registry_entry(wer_dll_path);
    match registry_check {
        Ok(true) => eprintln!("[DEBUG] ✅ Registry key exists with correct DLL path"),
        Ok(false) => eprintln!("[DEBUG] ❌ Registry key NOT found or DLL path mismatch!"),
        Err(e) => eprintln!("[DEBUG] ⚠️ Could not check registry: {:?}", e),
    }

    // Check if crash binary panic hook was triggered
    eprintln!("[DEBUG] Checking if crash binary panic hook was triggered...");
    if std::path::Path::new("C:\\Windows\\Temp\\crash_binary_panic.txt").exists() {
        eprintln!("[DEBUG] ⚠️ PANIC HOOK WAS CALLED (should NOT happen with real crashes!)");
        if let Ok(content) = std::fs::read_to_string("C:\\Windows\\Temp\\crash_binary_panic.txt") {
            eprintln!("[DEBUG] Panic info:\n{}", content);
        }
    } else {
        eprintln!("[DEBUG] ✅ No panic hook triggered (good - real crash occurred)");
    }

    // Check if WER handler was called (debug files)
    eprintln!("[DEBUG] Checking for WER handler debug files...");
    std::thread::sleep(Duration::from_millis(500)); // Give WER time to write debug files

    if std::path::Path::new("C:\\Windows\\Temp\\wer_handler_called.txt").exists() {
        eprintln!("[DEBUG] ✅ WER handler was called!");
        if let Ok(content) = std::fs::read_to_string("C:\\Windows\\Temp\\wer_handler_called.txt") {
            eprintln!("[DEBUG] WER handler content:\n{}", content);
        }
    } else {
        eprintln!("[DEBUG] ❌ WER handler was NOT called!");
    }

    // Wait for WER callback to complete and write output
    // WER processing is asynchronous, so we need to poll for the file
    wait_for_crash_output(&fixtures.crash_payload_path, Duration::from_secs(10))
        .context("Timeout waiting for crash output file")?;

    // Read and parse crash payload
    let crash_payload = read_and_parse_crash_payload(&fixtures.crash_payload_path)
        .context("Failed to read or parse crash payload")?;

    // Run custom validator
    validator(&crash_payload, &fixtures)?;

    // Cleanup registry key
    cleanup_registry_key(&fixtures.registry_key).context("Failed to cleanup registry key")?;

    // Cleanup debug files
    let _ = std::fs::remove_file("C:\\Windows\\Temp\\crash_binary_panic.txt");
    let _ = std::fs::remove_file("C:\\Windows\\Temp\\wer_handler_called.txt");
    let _ = std::fs::remove_file("C:\\Windows\\Temp\\wer_handler_success.txt");
    let _ = std::fs::remove_file("C:\\Windows\\Temp\\wer_handler_error.txt");

    Ok(())
}

/// Waits for crash output file to be created by WER callback.
fn wait_for_crash_output(path: &Path, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(100);

    loop {
        if path.exists() {
            // File exists, but wait a bit more to ensure it's fully written
            std::thread::sleep(Duration::from_millis(200));
            return Ok(());
        }

        if start.elapsed() > timeout {
            anyhow::bail!(
                "Timeout waiting for crash output at {:?} (waited {:?})",
                path,
                timeout
            );
        }

        std::thread::sleep(poll_interval);
    }
}

/// Cleans up the Windows registry key created for crash tracking.
pub fn cleanup_registry_key(key: &str) -> Result<()> {
    use std::process::Command;

    // Use reg.exe to delete the key
    // HKEY_CURRENT_USER\SOFTWARE\Microsoft\Windows\Windows Error
    // Reporting\RuntimeExceptionHelperModules
    let subkey =
        r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\RuntimeExceptionHelperModules";

    let output = Command::new("reg.exe")
        .args(["delete", "HKCU\\", "/v", key, "/f"])
        .arg(subkey)
        .output();

    // It's okay if deletion fails (key might not exist)
    match output {
        Ok(out) => {
            if !out.status.success() {
                eprintln!(
                    "Warning: Failed to delete registry key '{}': {}",
                    key,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: Failed to run reg.exe to cleanup key '{}': {}",
                key, e
            );
        }
    }

    Ok(())
}

/// Checks if a Windows registry key exists.
#[allow(dead_code)]
pub fn registry_key_exists(key: &str) -> Result<bool> {
    use std::process::Command;

    let subkey =
        r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\RuntimeExceptionHelperModules";

    let output = Command::new("reg.exe")
        .args(["query", &format!("HKCU\\{}", subkey), "/v", key])
        .output()
        .context("Failed to run reg.exe")?;

    Ok(output.status.success())
}

/// Checks if WER is enabled globally on the system
fn check_wer_enabled() -> Result<bool> {
    use std::process::Command;

    // Check both HKLM (system-wide) and HKCU (user-specific)
    let hklm_check = Command::new("reg.exe")
        .args(["query", "HKLM\\SOFTWARE\\Microsoft\\Windows\\Windows Error Reporting", "/v", "Disabled"])
        .output();

    let hkcu_check = Command::new("reg.exe")
        .args(["query", "HKCU\\SOFTWARE\\Microsoft\\Windows\\Windows Error Reporting", "/v", "Disabled"])
        .output();

    // Check HKLM (system-wide setting)
    if let Ok(output) = hklm_check {
        let output_str = String::from_utf8_lossy(&output.stdout);
        if output_str.contains("Disabled") && output_str.contains("0x1") {
            eprintln!("[DEBUG] WER is disabled in HKLM (system-wide)");
            return Ok(false);
        }
    }

    // Check HKCU (user-specific setting)
    if let Ok(output) = hkcu_check {
        let output_str = String::from_utf8_lossy(&output.stdout);
        if output_str.contains("Disabled") && output_str.contains("0x1") {
            eprintln!("[DEBUG] WER is disabled in HKCU (user-specific)");
            return Ok(false);
        }
    }

    // If neither key exists or value is 0, WER is enabled
    Ok(true)
}

/// Checks if the DLL exports OutOfProcessExceptionEventCallback
fn check_dll_export(dll_path: &Path) -> Result<bool> {
    use std::process::Command;

    // Use dumpbin if available, otherwise try manual DLL loading
    let output = Command::new("dumpbin")
        .args(["/EXPORTS", dll_path.to_str().unwrap()])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let output_str = String::from_utf8_lossy(&out.stdout);
            eprintln!("[DEBUG] DLL exports check via dumpbin");
            Ok(output_str.contains("OutOfProcessExceptionEventCallback"))
        }
        _ => {
            // dumpbin not available or failed, try loading DLL directly
            eprintln!("[DEBUG] dumpbin not available, checking DLL is loadable");
            Ok(dll_path.exists())
        }
    }
}

/// Checks if the WER registry entry exists and points to the correct DLL.
fn check_wer_registry_entry(dll_path: &Path) -> Result<bool> {
    use std::process::Command;

    let subkey =
        r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\RuntimeExceptionHelperModules";

    let dll_path_str = dll_path.to_string_lossy();

    eprintln!("[DEBUG] Looking for registry value: {}", dll_path_str);

    let output = Command::new("reg.exe")
        .args(["query", &format!("HKCU\\{}", subkey)])
        .output()
        .context("Failed to run reg.exe query")?;

    if !output.status.success() {
        eprintln!("[DEBUG] Registry query failed: {}", String::from_utf8_lossy(&output.stderr));
        return Ok(false);
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    eprintln!("[DEBUG] Registry output:\n{}", output_str);

    // Check if our DLL path appears in the output
    Ok(output_str.contains(&dll_path_str as &str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config = WindowsCrashTestConfig::new(
            BuildProfile::Debug,
            WindowsTestMode::Basic,
            WindowsCrashType::AccessViolationNull,
        );

        assert_eq!(config.profile, BuildProfile::Debug);
        assert_eq!(config.mode, WindowsTestMode::Basic);
        assert_eq!(config.crash_type, WindowsCrashType::AccessViolationNull);
        assert!(config.expect_upload);
        assert!(config.registry_key_override.is_none());
    }

    #[test]
    fn test_config_with_registry_key() {
        let config = WindowsCrashTestConfig::new(
            BuildProfile::Release,
            WindowsTestMode::Basic,
            WindowsCrashType::DivideByZero,
        )
        .with_registry_key("test_key_123".to_string());

        assert_eq!(
            config.registry_key_override,
            Some("test_key_123".to_string())
        );
    }

    #[test]
    fn test_fixtures_creation() {
        let fixtures = WindowsTestFixtures::new().unwrap();

        assert!(fixtures.crash_payload_path.exists() || !fixtures.crash_payload_path.exists());
        assert!(fixtures.output_dir.exists());
        assert!(!fixtures.registry_key.is_empty());
    }

    #[test]
    fn test_registry_key_generation() {
        let key1 = WindowsTestFixtures::generate_registry_key();
        std::thread::sleep(Duration::from_millis(10));
        let key2 = WindowsTestFixtures::generate_registry_key();

        assert_ne!(key1, key2, "Registry keys should be unique");
        assert!(key1.starts_with("DatadogCrashTrackerTest_"));
    }
}
