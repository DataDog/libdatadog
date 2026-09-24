// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    fileat_content_equals, handler_write_msg, remove_permissive, removeat_permissive,
    set_handler_path, wait_for_file_content, SIGNAL_HANDLER_TIMEOUT,
};

use libc;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::ffi::CString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicPtr;
// This is another SIGCHLD test and it is meant to validate the same thing, except that the child
// also performs an exec. This is a common case to support in some forked-worker pools where the
// child workers are spawned with fork+exec.

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
    handler_write_msg(&OUTPUT_FILE, b"O");
}

static OUTPUT_FILE: AtomicPtr<CString> = AtomicPtr::new(std::ptr::null_mut());

fn inner(output_dir: &Path, filename: &str) -> anyhow::Result<()> {
    // We're going to cause a SIGCHLD and check

    // Set the output file so the handler can pick up on it
    let ofile = output_dir.join(filename);
    set_handler_path(&OUTPUT_FILE, &ofile)?;

    // Use Command to launch a new instance of sh
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to execute command");

    // Wait for the child to finish
    let _ = child.wait();

    // Now check the output file.  Strongly assumes that nothing happened to change the value of
    // OUTPUT_FILE within the handler.  The handler runs asynchronously, so the file is waited on
    // rather than checked once.
    wait_for_file_content(&ofile, "O", SIGNAL_HANDLER_TIMEOUT)?;

    // Delete the file and the INVALID file to remove any previous state
    remove_permissive(&ofile);
    removeat_permissive(output_dir, "INVALID");

    // OK, we're done.  Return the output file name
    set_handler_path(&OUTPUT_FILE, &output_dir.join("INVALID"))
}

pub fn setup(output_dir: &Path) -> anyhow::Result<()> {
    // Create an empty sigset_t
    let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sigset);
    }

    // Set up the sigaction struct with sa_sigaction and sa_flags
    let sigchld_action = libc::sigaction {
        sa_sigaction: sigchld_handler as *const () as usize,
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
    set_handler_path(&OUTPUT_FILE, &output_dir.join("INVALID"))
}
