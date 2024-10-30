// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    atom_to_clone, does_file_contain_msg, file_append_msg, remove_file_permissive, set_atomic,
};

use datadog_crashtracker::CrashtrackerConfiguration;
use libc;
use nix::sys::socket;
use std::io::prelude::*;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicPtr;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &str,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.create_alt_stack = false;
        config.use_alt_stack = false;
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

extern "C" fn sigpipe_handler(_: libc::c_int) {
    // Open and write 'O' to the output file
    let ofile = match atom_to_clone(&OUTPUT_FILE) {
        Ok(f) => f,
        _ => return,
    };
    file_append_msg(&ofile, "O").ok();
}

static OUTPUT_FILE: AtomicPtr<String> = AtomicPtr::new(std::ptr::null_mut());

fn inner(output_dir: &str, filename: &str) -> anyhow::Result<()> {
    // We're going to cause a SIGPIPE and check

    // Set the output file so the handler can pick up on it
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/{filename}"));
    let ofile = atom_to_clone(&OUTPUT_FILE)?;

    // Cause a SIGPIPE to occur by opening a socketpair, closing the read side, and writing into
    // the write side.
    let (reader_fd, writer_fd) = socket::socketpair(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        None,
        socket::SockFlag::empty(),
    )?;
    drop(reader_fd);
    let mut writer = UnixStream::from(writer_fd);
    if writer.write_all(b"Hello").is_ok() {
        anyhow::bail!("Expected write to fail, but it succeeded");
    }

    // Now check the output file.  Strongly assumes that nothing happened to change the value of
    // OUTPUT_FILE within the handler.
    match does_file_contain_msg(&ofile, "O") {
        Ok(true) => (), // Expected, do nothing
        _ => {
            anyhow::bail!("Output file was not 'O'");
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
    let sigpipe_action = libc::sigaction {
        sa_sigaction: sigpipe_handler as usize,
        sa_mask: sigset,
        sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
        sa_restorer: None,
    };

    // Register the handler for SIGPIPE
    unsafe {
        if libc::sigaction(libc::SIGPIPE, &sigpipe_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGPIPE handler");
        }
    }

    // Additional setup logic for the output file
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/INVALID"));

    Ok(())
}
