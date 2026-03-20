// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Integration test for SaGuard during crash handling.
//
// Verifies that SIGCHLD and SIGPIPE handlers installed by the application are
// not invoked during crash handling (because SaGuard suppresses them), even
// though the crash handler spawns child processes (SIGCHLD) and writes to
// pipes (SIGPIPE).
//
// Expected operation:
// 1. setup() installs custom SIGCHLD and SIGPIPE handlers that write marker files when invoked
// 2. pre() verifies the handlers actually work as a baseline check
// 3. post() cleans up marker files and sets the output targets to "crash_sigchld" and
//    "crash_sigpipe" files. If the SaGuard is working, the crash handler will suppress these
//    signals and the marker files will not be created
//
// The integration test asserts that "crash_sigchld" and "crash_sigpipe" do
// not exist after the crash
use crate::modes::behavior::Behavior;
use crate::modes::behavior::{
    atom_to_clone, file_write_msg, fileat_content_equals, remove_permissive, removeat_permissive,
    set_atomic, trigger_sigpipe,
};

use libc;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicPtr;

pub const CRASH_SIGCHLD_FILENAME: &str = "crash_sigchld";
pub const CRASH_SIGPIPE_FILENAME: &str = "crash_sigpipe";

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
        // verify SIGCHLD handler fires
        verify_sigchld(output_dir, "pre_sigchld.check")?;

        // verify SIGPIPE handler fires
        verify_sigpipe(output_dir, "pre_sigpipe.check")?;

        Ok(())
    }

    fn post(&self, output_dir: &Path) -> anyhow::Result<()> {
        removeat_permissive(output_dir, CRASH_SIGCHLD_FILENAME);
        removeat_permissive(output_dir, CRASH_SIGPIPE_FILENAME);

        // Point the handlers at the crash-time marker files
        // If SaGuard works, these files wont be created during crash handling
        set_atomic(
            &SIGCHLD_OUTPUT_FILE,
            output_dir.join(CRASH_SIGCHLD_FILENAME),
        );
        set_atomic(
            &SIGPIPE_OUTPUT_FILE,
            output_dir.join(CRASH_SIGPIPE_FILENAME),
        );
        Ok(())
    }
}

static SIGCHLD_OUTPUT_FILE: AtomicPtr<PathBuf> = AtomicPtr::new(std::ptr::null_mut());
static SIGPIPE_OUTPUT_FILE: AtomicPtr<PathBuf> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn sigchld_handler(_: libc::c_int) {
    let ofile = match atom_to_clone(&SIGCHLD_OUTPUT_FILE) {
        Ok(f) => f,
        _ => return,
    };
    file_write_msg(&ofile, "SIGCHLD_FIRED").ok();
}

extern "C" fn sigpipe_handler(_: libc::c_int) {
    let ofile = match atom_to_clone(&SIGPIPE_OUTPUT_FILE) {
        Ok(f) => f,
        _ => return,
    };
    file_write_msg(&ofile, "SIGPIPE_FIRED").ok();
}

fn verify_sigchld(output_dir: &Path, filename: &str) -> anyhow::Result<()> {
    set_atomic(&SIGCHLD_OUTPUT_FILE, output_dir.join(filename));

    match unsafe { libc::fork() } {
        -1 => anyhow::bail!("Failed to fork"),
        0 => unsafe {
            libc::_exit(0);
        },
        _ => {
            // Wait for child to exit
            loop {
                let mut status: libc::c_int = 0;
                if -1 == unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) } {
                    break;
                }
            }
        }
    }

    match fileat_content_equals(output_dir, filename, "SIGCHLD_FIRED") {
        Ok(true) => (),
        _ => anyhow::bail!("SIGCHLD handler did not fire during baseline check"),
    }

    remove_permissive(&output_dir.join(filename));
    set_atomic(&SIGCHLD_OUTPUT_FILE, output_dir.join("INVALID"));
    Ok(())
}

fn verify_sigpipe(output_dir: &Path, filename: &str) -> anyhow::Result<()> {
    set_atomic(&SIGPIPE_OUTPUT_FILE, output_dir.join(filename));

    trigger_sigpipe()?;

    match fileat_content_equals(output_dir, filename, "SIGPIPE_FIRED") {
        Ok(true) => (),
        _ => anyhow::bail!("SIGPIPE handler did not fire during baseline check"),
    }

    remove_permissive(&output_dir.join(filename));
    set_atomic(&SIGPIPE_OUTPUT_FILE, output_dir.join("INVALID"));
    Ok(())
}

pub fn setup(output_dir: &Path) -> anyhow::Result<()> {
    let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sigset);
    }

    // Install SIGCHLD handler
    let sigchld_action = libc::sigaction {
        sa_sigaction: sigchld_handler as *const () as usize,
        sa_mask: sigset,
        sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
        #[cfg(target_os = "linux")]
        sa_restorer: None,
    };
    unsafe {
        if libc::sigaction(libc::SIGCHLD, &sigchld_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGCHLD handler");
        }
    }

    // Install SIGPIPE handler
    let sigpipe_action = libc::sigaction {
        sa_sigaction: sigpipe_handler as *const () as usize,
        sa_mask: sigset,
        sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
        #[cfg(target_os = "linux")]
        sa_restorer: None,
    };
    unsafe {
        if libc::sigaction(libc::SIGPIPE, &sigpipe_action, std::ptr::null_mut()) != 0 {
            anyhow::bail!("Failed to set up SIGPIPE handler");
        }
    }

    // Initialize output file pointers to INVALID
    set_atomic(&SIGCHLD_OUTPUT_FILE, output_dir.join("INVALID"));
    set_atomic(&SIGPIPE_OUTPUT_FILE, output_dir.join("INVALID"));

    Ok(())
}
