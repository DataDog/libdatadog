// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    atom_to_clone, does_file_contain_msg, file_append_msg, set_atomic, remove_file_permissive,
};

use datadog_crashtracker::CrashtrackerConfiguration;
use libc;
use std::sync::atomic::AtomicPtr;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &str,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        setup(output_dir)
    }

    fn pre(&self, output_dir: &str) -> anyhow::Result<()> {
        inner(output_dir, "pre.check")
    }

    fn post(&self, output_dir: &str) -> anyhow::Result<()> {
        // Before anything, check the pretest file
        match does_file_contain_msg(&format!("{output_dir}/INVALID"), "O") {
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

static OUTPUT_FILE: AtomicPtr<String> = AtomicPtr::new(std::ptr::null_mut());

fn inner(output_dir: &str, filename: &str) -> anyhow::Result<()> {
    // We're going to cause a SIGCHLD and check

    // Set the output file so the handler can pick up on it
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/{filename}"));
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
    match does_file_contain_msg(&ofile, "O") {
        Ok(true) => (), // Expected, do nothing
        _ => {
            anyhow::bail!("Output file {}/{} was not 'O'", output_dir, filename);
        }
    }

    // Delete the file and the INVALID file to remove any previous state
    remove_file_permissive(&ofile);
    remove_file_permissive(&format!("{output_dir}/INVALID"));

    // OK, we're done.  Return the output file name
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/INVALID"));
    Ok(())
}

pub fn setup(output_dir: &str) -> anyhow::Result<()> {
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
        sa_restorer: None,
    };

    // Register the handler for SIGCHLD
    unsafe {
        if libc::sigaction(libc::SIGCHLD, &sigchld_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGCHLD handler");
        }
    }

    // Additional setup logic for the output file
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/INVALID"));

    Ok(())
}
