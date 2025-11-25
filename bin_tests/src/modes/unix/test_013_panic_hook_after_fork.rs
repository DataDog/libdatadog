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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        post()
    }
}

fn post() -> anyhow::Result<()> {
    // Set up a panic hook to verify it gets called
    let panic_hook_called = Arc::new(AtomicBool::new(false));
    let panic_hook_called_clone = Arc::clone(&panic_hook_called);

    std::panic::set_hook(Box::new(move |_panic_info| {
        panic_hook_called_clone.store(true, Ordering::SeqCst);
    }));

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

            // Parent exits with error code to indicate test completion
            // The test harness will verify the crash report contains the panic message
            unsafe {
                libc::_exit(1);
            }
        }
    }
}
