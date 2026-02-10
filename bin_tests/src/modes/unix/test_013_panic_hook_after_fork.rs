// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Test that panic hooks registered before fork() continue to work in child processes.
// This validates that:
// 1. The panic hook survives fork()
// 2. The panic message is captured in the child process
// 3. The crash report is correctly generated
use crate::modes::behavior::Behavior;
use libdd_crashtracker::{self as crashtracker, CrashtrackerConfiguration};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pre(&self, output_dir: &Path) -> anyhow::Result<()> {
        pre(output_dir)
    }

    fn post(&self, output_dir: &Path) -> anyhow::Result<()> {
        post(output_dir)
    }
}

fn pre(output_dir: &Path) -> anyhow::Result<()> {
    let old_hook = std::panic::take_hook();
    let output_dir = output_dir.to_path_buf();

    // Set up a panic hook BEFORE crashtracker::init to verify the hook chain works
    std::panic::set_hook(Box::new(move |panic_info| {
        // Mark that our custom hook was called by writing a marker file
        // This works across fork() because it's persistent storage
        let marker_path = output_dir.join("panic_hook_called.marker");
        let _ = fs::write(marker_path, "hook was called");

        // Call the previous hook (usually the default panic hook)
        old_hook(panic_info);
    }));

    Ok(())
}

fn post(output_dir: &Path) -> anyhow::Result<()> {
    match unsafe { libc::fork() } {
        -1 => {
            anyhow::bail!("Failed to fork");
        }
        0 => {
            // Child - panic with a specific message
            // The crashtracker should capture both the panic hook execution
            // and the panic message
            crashtracker::begin_op(crashtracker::OpTypes::ProfilerCollectingSample)?;

            // Give parent time to set up wait
            std::thread::sleep(Duration::from_millis(10));

            panic!("child panicked after fork - hook should fire");
        }
        pid => {
            // Parent - wait for child to panic and crash
            let start_time = Instant::now();
            let max_wait = Duration::from_secs(5);

            loop {
                match waitpid(Pid::from_raw(pid), None)? {
                    WaitStatus::StillAlive => {
                        if start_time.elapsed() > max_wait {
                            anyhow::bail!("Child process did not exit within 5 seconds");
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    WaitStatus::Exited(_pid, exit_code) => {
                        // Child exited - this is what we expect after panic
                        eprintln!("Child exited with code: {}", exit_code);
                        break;
                    }
                    WaitStatus::Signaled(_pid, signal, _) => {
                        // Child was killed by signal (also acceptable for panic)
                        eprintln!("Child killed by signal: {:?}", signal);
                        break;
                    }
                    _ => {
                        // Other status - continue waiting
                    }
                }
            }

            // Verify that our custom panic hook was called by checking for the marker file
            // This proves that the hook chain works correctly:
            // crashtracker's hook -> our custom hook -> default hook
            let marker_path = output_dir.join("panic_hook_called.marker");

            if !marker_path.exists() {
                anyhow::bail!(
                    "Custom panic hook was not called - hook chaining failed! \
                    Expected marker file at: {}",
                    marker_path.display()
                );
            }

            // Parent exits with error code to indicate test completion
            // The test harness will verify the crash report contains the panic message
            unsafe {
                libc::_exit(1);
            }
        }
    }
}
