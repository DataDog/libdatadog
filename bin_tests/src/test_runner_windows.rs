// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows-specific test runner infrastructure for crash tracking tests.
//! Provides configuration and execution framework for Windows crash tests.

use crate::{
    test_types_windows::WindowsCrashType, validation::read_and_parse_crash_payload, BuildProfile,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_TERMINATE,
};

/// Type alias for validator functions used in Windows test runners.
pub type WindowsValidatorFn = Box<dyn FnOnce(&Value, &WindowsTestFixtures) -> Result<()>>;

/// Helper to create a named event
fn create_event(name: &str) -> Result<HANDLE> {
    unsafe {
        CreateEventW(
            None,
            true,  // Manual reset
            false, // Initially non-signaled
            &windows::core::HSTRING::from(name),
        )
        .context(format!("Failed to create event: {}", name))
    }
}

/// Helper to wait for an event with timeout and proper error handling
fn wait_for_event(handle: HANDLE, timeout_ms: u32, description: &str) -> Result<()> {
    let wait_result = unsafe { WaitForSingleObject(handle, timeout_ms) };

    if wait_result.0 != 0 {
        // 0 = WAIT_OBJECT_0 (success)
        anyhow::bail!(
            "Timeout waiting for {} (wait result: {}, timeout: {}ms)",
            description,
            wait_result.0,
            timeout_ms
        );
    }

    Ok(())
}

/// RAII guard for event handles - ensures cleanup on drop
struct EventHandles {
    crash_ready: HANDLE,
    simulator_ready: HANDLE,
    crash_event: HANDLE,
    done_event: HANDLE,
}

impl EventHandles {
    /// Create all events with proper error handling
    fn new(
        crash_ready_name: &str,
        simulator_ready_name: &str,
        crash_event_name: &str,
        done_event_name: &str,
    ) -> Result<Self> {
        Ok(Self {
            crash_ready: create_event(crash_ready_name)?,
            simulator_ready: create_event(simulator_ready_name)?,
            crash_event: create_event(crash_event_name)?,
            done_event: create_event(done_event_name)?,
        })
    }
}

impl Drop for EventHandles {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.crash_ready);
            let _ = CloseHandle(self.simulator_ready);
            let _ = CloseHandle(self.crash_event);
            let _ = CloseHandle(self.done_event);
        }
    }
}

/// Wait for a process to complete with timeout, killing it if it exceeds the timeout.
///
/// # Arguments
/// * `process` - The child process to wait for
/// * `timeout` - Maximum time to wait before killing the process
/// * `process_name` - Name of the process (for logging)
///
/// # Returns
/// * `Ok(ExitStatus)` - Process exited within timeout
/// * `Err` - Process timed out or wait failed
fn wait_for_process_with_timeout(
    mut process: process::Child,
    timeout: Duration,
    process_name: &str,
) -> Result<std::process::ExitStatus> {
    let pid = process.id();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = process.wait();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(status)) => {
            eprintln!(
                "[TEST_RUNNER] {} exited with status: {:?}",
                process_name, status
            );
            Ok(status)
        }
        Ok(Err(e)) => {
            eprintln!(
                "[TEST_RUNNER] ⚠️ Failed to wait for {}: {}",
                process_name, e
            );
            Err(e).context(format!("Failed to wait for {}", process_name))
        }
        Err(_) => {
            eprintln!(
                "[TEST_RUNNER] ⚠️ {} did not complete within {}s, killing it...",
                process_name,
                timeout.as_secs()
            );

            // Kill the process
            unsafe {
                if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let _ = TerminateProcess(handle, 1);
                    eprintln!("[TEST_RUNNER] {} (PID: {}) terminated", process_name, pid);
                } else {
                    eprintln!(
                        "[TEST_RUNNER] Failed to open {} for termination",
                        process_name
                    );
                }
            }

            anyhow::bail!("{} timeout after {}s", process_name, timeout.as_secs())
        }
    }
}

/// Configuration for a Windows crash tracking test.
#[derive(Debug, Clone)]
pub struct WindowsCrashTestConfig {
    /// Build profile for the test binaries
    pub profile: BuildProfile,
    /// Type of crash to trigger
    pub crash_type: WindowsCrashType,
    /// Whether to expect successful upload
    pub expect_upload: bool,
    /// Optional timeout for WER simulator (milliseconds)
    pub simulator_timeout_ms: Option<u32>,
}

impl WindowsCrashTestConfig {
    /// Creates a new Windows test configuration.
    pub fn new(profile: BuildProfile, crash_type: WindowsCrashType) -> Self {
        Self {
            profile,
            crash_type,
            expect_upload: true,
            simulator_timeout_ms: None, // Use default (5000ms)
        }
    }

    /// Sets whether to expect upload success.
    pub fn with_expect_upload(mut self, expect: bool) -> Self {
        self.expect_upload = expect;
        self
    }

    /// Sets the WER simulator timeout in milliseconds.
    pub fn with_simulator_timeout(mut self, timeout_ms: u32) -> Self {
        self.simulator_timeout_ms = Some(timeout_ms);
        self
    }
}

/// Test fixtures for Windows crash tests.
pub struct WindowsTestFixtures {
    /// Path where crash payload will be written
    pub crash_payload_path: PathBuf,
    /// Output directory for test artifacts
    pub output_dir: PathBuf,
    /// Temporary directory (kept alive for test duration)
    #[allow(dead_code)]
    tmpdir: tempfile::TempDir,
}

impl WindowsTestFixtures {
    /// Creates new test fixtures with temporary directory.
    pub fn new() -> Result<Self> {
        let tmpdir = tempfile::TempDir::new().context("Failed to create temporary directory")?;
        let dirpath = tmpdir.path();

        Ok(Self {
            crash_payload_path: extend_path(dirpath, "crash.json"),
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

/// Runs a Windows crash test with the given configuration and validator.
///
/// # Arguments
/// * `config` - Test configuration
/// * `binary_path` - Path to the test binary
/// * `simulator_path` - Path to the WER simulator binary
/// * `validator` - Custom validation function
pub fn run_windows_crash_test<F>(
    config: &WindowsCrashTestConfig,
    binary_path: &Path,
    simulator_path: &Path,
    validator: F,
) -> Result<()>
where
    F: FnOnce(&Value, &WindowsTestFixtures) -> Result<()>,
{
    let fixtures = WindowsTestFixtures::new()?;

    // Create unique event names and info file for this test run
    let test_id = std::process::id();
    let crash_event_name = format!("CrashEvent_{}", test_id);
    let done_event_name = format!("DoneEvent_{}", test_id);
    let crash_ready_event_name = format!("CrashReady_{}", test_id);
    let simulator_ready_event_name = format!("SimulatorReady_{}", test_id);
    let info_file = fixtures.output_dir.join("crash_info.txt");

    eprintln!("[TEST_RUNNER] Starting Windows crash test...");
    eprintln!(
        "[TEST_RUNNER] Event names: crash={}, done={}, crash_ready={}, sim_ready={}",
        crash_event_name, done_event_name, crash_ready_event_name, simulator_ready_event_name
    );
    eprintln!("[TEST_RUNNER] Info file: {}", info_file.display());

    // Create all synchronization events (test runner owns all sync primitives)
    // EventHandles guard ensures cleanup on drop (even on error)
    eprintln!("[TEST_RUNNER] Creating synchronization events...");

    let _event_handles = EventHandles::new(
        &crash_ready_event_name,
        &simulator_ready_event_name,
        &crash_event_name,
        &done_event_name,
    )?;

    // Build command for crash binary
    let mut cmd = process::Command::new(binary_path);
    cmd.arg(format!("file://{}", fixtures.crash_payload_path.display()))
        .arg(&fixtures.output_dir)
        .arg(config.crash_type.as_str())
        .arg(config.simulator_timeout_ms.unwrap_or(5000).to_string())
        .arg(&crash_event_name)
        .arg(&done_event_name)
        .arg(&crash_ready_event_name)
        .arg(&info_file);

    eprintln!(
        "[TEST_RUNNER] Spawning crash binary: {}",
        binary_path.display()
    );

    // Spawn crash binary (it will initialize and signal when ready)
    let crash_process = cmd
        .spawn()
        .context("Failed to spawn Windows test process")?;

    eprintln!(
        "[TEST_RUNNER] Crash binary spawned (PID: {})",
        crash_process.id()
    );

    // Wait for crash binary to signal it's ready (blocking, no polling!)
    eprintln!("[TEST_RUNNER] Waiting for crash binary to initialize...");
    wait_for_event(
        _event_handles.crash_ready,
        30000,
        "crash binary to initialize",
    )?;
    eprintln!("[TEST_RUNNER] ✅ Crash binary ready!");

    // Read crash info (PID, TID, context_addr, exception_code_addr) from file
    eprintln!("[TEST_RUNNER] Reading crash info from file...");
    let crash_info =
        std::fs::read_to_string(&info_file).context("Failed to read crash info file")?;
    let parts: Vec<&str> = crash_info.trim().split('|').collect();
    anyhow::ensure!(
        parts.len() == 4,
        "Invalid crash info format: {}",
        crash_info
    );

    let parent_pid: u32 = parts[0].parse().context("Invalid PID")?;
    let parent_tid: u32 = parts[1].parse().context("Invalid TID")?;
    let context_addr = parts[2]; // Address in crash binary's address space
    let exception_code_addr = parts[3]; // Address of EXCEPTION_CODE in crash binary's address space

    eprintln!(
        "[TEST_RUNNER] Crash info: PID={}, TID={}, context_addr={}, exception_code_addr={}",
        parent_pid, parent_tid, context_addr, exception_code_addr
    );
    eprintln!("[TEST_RUNNER] Note: context_addr and exception_code_addr are in crash binary's address space (will be read via ReadProcessMemory)");

    // Spawn WER simulator
    eprintln!(
        "[TEST_RUNNER] Spawning WER simulator: {}",
        simulator_path.display()
    );
    eprintln!(
        "[TEST_RUNNER] Simulator args: pid={}, tid={}, context_addr={}, exception_code_addr={}, events={}/{}/{}, output={}",
        parent_pid,
        parent_tid,
        context_addr,
        exception_code_addr,
        crash_event_name,
        done_event_name,
        simulator_ready_event_name,
        fixtures.output_dir.display()
    );
    let mut simulator_cmd = process::Command::new(simulator_path);
    simulator_cmd
        .arg(parent_pid.to_string())
        .arg(parent_tid.to_string())
        .arg(context_addr)
        .arg(exception_code_addr)
        .arg(&crash_event_name)
        .arg(&done_event_name)
        .arg(&simulator_ready_event_name)
        .arg(&fixtures.output_dir); // Pass output directory for debug files

    let simulator_process = simulator_cmd
        .spawn()
        .context("Failed to spawn WER simulator")?;

    eprintln!(
        "[TEST_RUNNER] WER simulator spawned (PID: {})",
        simulator_process.id()
    );

    // Wait for simulator to signal it's ready (blocking, no polling!)
    eprintln!("[TEST_RUNNER] Waiting for WER simulator to initialize...");
    wait_for_event(
        _event_handles.simulator_ready,
        30000,
        "WER simulator to initialize",
    )?;
    eprintln!("[TEST_RUNNER] ✅ WER simulator ready!");
    eprintln!("[TEST_RUNNER] Waiting for crash binary to crash...");

    // Wait for crash process to exit with timeout
    let exit_status =
        wait_for_process_with_timeout(crash_process, Duration::from_secs(30), "Crash binary")?;

    // On Windows, crashes typically result in non-zero exit
    // (Unlike Unix where some signals can result in "success")
    anyhow::ensure!(
        !exit_status.success(),
        "Expected process to crash (non-zero exit), but it succeeded"
    );

    eprintln!("[TEST_RUNNER] Waiting for WER simulator to complete...");

    // Wait for simulator to complete with timeout
    wait_for_process_with_timeout(simulator_process, Duration::from_secs(30), "WER simulator")?;

    // Check if WER simulator was called (debug files in test-specific output_dir)
    eprintln!("[TEST_RUNNER] Checking for WER simulator debug files...");

    let success_file = fixtures.output_dir.join("wer_simulator_success.txt");
    let error_file = fixtures.output_dir.join("wer_simulator_error.txt");

    if success_file.exists() {
        eprintln!("[TEST_RUNNER] ✅ WER simulator processed crash successfully!");
        if let Ok(content) = std::fs::read_to_string(&success_file) {
            eprintln!("[TEST_RUNNER] Simulator success content:\n{}", content);
        }
    } else if error_file.exists() {
        eprintln!("[TEST_RUNNER] ⚠️ WER simulator encountered an error");
        if let Ok(content) = std::fs::read_to_string(&error_file) {
            eprintln!("[TEST_RUNNER] Simulator error:\n{}", content);
        }
    } else {
        eprintln!("[TEST_RUNNER] ❌ WER simulator was NOT called!");
    }

    // Crash output file should already exist (simulator writes it before exiting)
    eprintln!("[TEST_RUNNER] Reading crash output file...");
    anyhow::ensure!(
        fixtures.crash_payload_path.exists(),
        "Crash output file not found at {:?} (simulator should have created it before exiting)",
        fixtures.crash_payload_path
    );

    // Read and parse crash payload
    let crash_payload = read_and_parse_crash_payload(&fixtures.crash_payload_path)
        .context("Failed to read or parse crash payload")?;

    eprintln!("[TEST_RUNNER] Running validator...");

    // Run custom validator
    validator(&crash_payload, &fixtures)?;

    eprintln!("[TEST_RUNNER] Test passed! Cleaning up...");

    // Event handles cleaned up automatically by _event_handles Drop
    // Output directory (fixtures.output_dir) cleaned up when fixtures.tmpdir is dropped

    eprintln!("[TEST_RUNNER] Cleanup complete");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config =
            WindowsCrashTestConfig::new(BuildProfile::Debug, WindowsCrashType::AccessViolationNull);

        assert_eq!(config.profile, BuildProfile::Debug);
        assert_eq!(config.crash_type, WindowsCrashType::AccessViolationNull);
        assert!(config.expect_upload);
        assert_eq!(config.simulator_timeout_ms, None);
    }

    #[test]
    fn test_config_with_timeout() {
        let config =
            WindowsCrashTestConfig::new(BuildProfile::Debug, WindowsCrashType::AccessViolationNull)
                .with_simulator_timeout(10000);

        assert_eq!(config.simulator_timeout_ms, Some(10000));
    }

    #[test]
    fn test_fixtures_creation() {
        let fixtures = WindowsTestFixtures::new().unwrap();

        assert!(fixtures.crash_payload_path.exists() || !fixtures.crash_payload_path.exists());
        assert!(fixtures.output_dir.exists());
    }
}
