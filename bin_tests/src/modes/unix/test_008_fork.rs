// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// The purpose of this test is to show that the crashtracker can continue to operate correctly when
// instrumentation is initialized in a parent process, but a crash happens in a child process.
// This is a fairly common situation for runtimes that use forked worker-pools, such as Python.
// - Forks a child process in `post()` (no need to do any setup or pre-init stuff)
// - Child will segfault
// - Parent waits for the child to exit, then calls _exit
// - If the crashtracking data was received, then yay it worked!
use crate::modes::behavior::Behavior;
use datadog_crashtracker::{self as crashtracker, CrashtrackerConfiguration};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
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

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        post()
    }
}

#[inline(never)]
unsafe fn deref_ptr(ptr: *mut u8) {
    // Do the segfault in as identical as possible a manner as the main test
    *std::hint::black_box(ptr) = std::hint::black_box(1);
}

fn post() -> anyhow::Result<()> {
    match unsafe { libc::fork() } {
        -1 => {
            anyhow::bail!("Failed to fork");
        }
        0 => {
            // Child
            // The test assumes that we've incremented the op counter, and it's cumbersome to try
            // and generalize the test in a different direction, so we just hit it here.
            crashtracker::begin_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
            unsafe {
                deref_ptr(std::ptr::null_mut::<u8>());
            }
        }
        pid => {
            // Parent
            let start_time = Instant::now();
            let max_wait = Duration::from_millis(1_000);
            while let WaitStatus::StillAlive = waitpid(Pid::from_raw(pid), None)? {
                if start_time.elapsed() > max_wait {
                    anyhow::bail!("Child process did not exit within 1 second");
                }
            }

            // Either the child finished and we're good, or it didn't finish.  Either way, we don't
            // want to cause a segfault ourselves, so let's get out of here.  Also note that we
            // have to exit with a non-success code, since the harness looks for that.
            unsafe {
                libc::_exit(1);
            }
        }
    }

    Ok(())
}
