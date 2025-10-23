// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test is similar to the other SIGPIPE test, except it ensures that everything works
// correctly when using the regular signal stack. Why two tests?
// - The altstack is a little bit different from the regular one, so might as well check both
// - These different stacks might have different size limits
// It would be great to harmonize these tests in some way, but I was unable to do so with the time
// I had.
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    atom_to_clone, file_append_msg, file_content_equals, fileat_content_equals, remove_permissive,
    removeat_permissive, set_atomic, trigger_sigpipe,
};

use libc;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicPtr;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.set_create_alt_stack(false)?;
        config.set_use_alt_stack(false)?;
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

extern "C" fn sigpipe_handler(_: libc::c_int) {
    // Open and write 'O' to the output file
    let ofile = match atom_to_clone(&OUTPUT_FILE) {
        Ok(f) => f,
        _ => return,
    };
    file_append_msg(&ofile, "O").ok();
}

static OUTPUT_FILE: AtomicPtr<PathBuf> = AtomicPtr::new(std::ptr::null_mut());

fn inner(output_dir: &Path, filename: &str) -> anyhow::Result<()> {
    // We're going to cause a SIGPIPE and check

    // Set the output file so the handler can pick up on it
    set_atomic(&OUTPUT_FILE, output_dir.join(filename));
    let ofile = atom_to_clone(&OUTPUT_FILE)?;

    trigger_sigpipe()?;

    // Now check the output file.  Strongly assumes that nothing happened to change the value of
    // OUTPUT_FILE within the handler.
    match file_content_equals(&ofile, "O") {
        Ok(true) => (), // Expected, do nothing
        _ => {
            anyhow::bail!("Output file was not 'O'");
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
    let sigpipe_action = libc::sigaction {
        sa_sigaction: sigpipe_handler as usize,
        sa_mask: sigset,
        sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
        #[cfg(target_os = "linux")]
        sa_restorer: None,
    };

    // Register the handler for SIGPIPE
    unsafe {
        if libc::sigaction(libc::SIGPIPE, &sigpipe_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGPIPE handler");
        }
    }

    // Switch the output file to INVALID, so if the handler runs, we will see that it was run
    // erroneously
    set_atomic(&OUTPUT_FILE, output_dir.join("INVALID"));

    Ok(())
}
