// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This test checks that SIGCHLD isn't emitted during the initialization of the crashtracker and
// also checks that during a crash a SIGCHLD isn't erroneously handled by the main application.
// This test combines a combination of checks.
// - `setup` creates a SIGCHLD handler and sets the output file to `INVALID`. Generally speaking,
//   any write to `INVALID` creates an error condition.  The SIGCHLD handler writes to the current
//   file, so this is a form of state checking.
// - `pre` verifies that the SIGCHLD handler is working as expected (otherwise the test could
//   erroneously _pass_) When the crashtracker is initialized, it should not trigger the SIGCHLD
//   handler.  The `INVALID` file is checked at the top of the `post` check to ensure against this.
// - `post` once again verifies that after installing the crashtracker, a SIGCHLD handler continues
//   to work properly.
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    atom_to_clone, file_append_msg, fileat_content_equals, remove_permissive, removeat_permissive,
    set_atomic,
};

use datadog_crashtracker::CrashtrackerConfiguration;
use libc;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicPtr;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        setup(output_dir)
    }

    fn pre(&self, output_dir: &Path) -> anyhow::Result<()> {
        inner(output_dir, "pre.check")
    }

    fn post(&self, output_dir: &Path) -> anyhow::Result<()> {
        // Before anything, check the pretest file
        match fileat_content_equals(output_dir, "INVALID", "O") {
            Ok(true) | Ok(false) => {
                // File is deleted at the end of `inner`, so if it exists, it's an error
                anyhow::bail!("INVALID file should not exist");
            }
            _ => (), // Anything else is fine
        }

        // Run the test
        inner(output_dir, "post.check")
    }
}

extern "C" fn sigchld_handler(_: libc::c_int) {
    // Open and write 'O' to the output file
    let ofile = match atom_to_clone(&OUTPUT_FILE) {
        Ok(ofile) => ofile,
        _ => {
            return;
        }
    };
    file_append_msg(&ofile, "O").ok();
}

static OUTPUT_FILE: AtomicPtr<PathBuf> = AtomicPtr::new(std::ptr::null_mut());

fn inner(output_dir: &Path, filename: &str) -> anyhow::Result<()> {
    // We're going to cause a SIGCHLD and check

    // Set the output file so the handler can pick up on it
    set_atomic(&OUTPUT_FILE, output_dir.join(filename));
    let ofile = atom_to_clone(&OUTPUT_FILE)?;

    match unsafe { libc::fork() } {
        -1 => {
            anyhow::bail!("Failed to fork");
        }
        0 => {
            // Child process
            unsafe { libc::_exit(0) };
        }
        _ => {
            // Sit in a while loop, doing nonblocking waits for the child to exit
            loop {
                let mut status: libc::c_int = 0;
                // Checking the return status of a child process is a little fiddly, since
                // `waitpid` actually just tracks "changes" to state, so just keep checking until
                // we get a -1, which almost universally means the child has exited.
                // There's no reason to get defensive and do a timed check, since the flow control
                // for the child process is pretty straightforward.
                if -1 == unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) } {
                    break;
                }
            }
        }
    }

    // Now check the output file.  Strongly assumes that nothing happened to change the value of
    // OUTPUT_FILE within the handler.
    match fileat_content_equals(output_dir, filename, "O") {
        Ok(true) => (), // Expected, do nothing
        _ => {
            anyhow::bail!("Output file {:?}/{} was not 'O'", output_dir, filename);
        }
    }

    // Delete the file and the INVALID file to remove any previous state
    remove_permissive(&ofile);
    removeat_permissive(output_dir, "INVALID");

    // OK, we're done.  Return the output file name
    set_atomic(&OUTPUT_FILE, output_dir.join("INVALID"));
    Ok(())
}

pub fn setup(output_dir: &Path) -> anyhow::Result<()> {
    // Create an empty sigset_t
    let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sigset);
    }

    // Set up the sigaction struct with sa_sigaction and sa_flags
    let sigchld_action = libc::sigaction {
        sa_sigaction: sigchld_handler as usize,
        sa_mask: sigset,
        sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
        #[cfg(target_os = "linux")]
        sa_restorer: None,
    };

    // Register the handler for SIGCHLD
    unsafe {
        if libc::sigaction(libc::SIGCHLD, &sigchld_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGCHLD handler");
        }
    }

    // Additional setup logic for the output file
    set_atomic(&OUTPUT_FILE, output_dir.join("INVALID"));

    Ok(())
}
