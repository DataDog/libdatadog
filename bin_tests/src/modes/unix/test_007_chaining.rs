// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;

use crate::modes::behavior::{atom_to_clone, file_append_msg, remove_file_permissive, set_atomic};
use datadog_crashtracker::CrashtrackerConfiguration;
use libc;
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use std::sync::atomic::{AtomicPtr, Ordering::SeqCst};

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &str,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pre(&self, _output_dir: &str) -> anyhow::Result<()> {
        // There's no pretest since we don't really want to write a complex segfault handler with
        // recovery
        Ok(())
    }

    fn post(&self, output_dir: &str) -> anyhow::Result<()> {
        // Test to make sure the crashtracker can be chained without issue.
        // This DOES NOT test that the crashtracker's chaining mechanism works.
        test(output_dir)
    }
}

static OUTPUT_FILE: AtomicPtr<String> = AtomicPtr::new(std::ptr::null_mut());
static OLD_ACTION: AtomicPtr<SigAction> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn segfault_sigaction(
    signum: i32,
    sig_info: *mut libc::siginfo_t,
    ucontext: *mut libc::c_void,
) {
    // Set up the chaining with the old handler.  This is written in a generic way, emulating how
    // chaining is frequently done in the wild.
    let old_action = match atom_to_clone(&OLD_ACTION) {
        Ok(a) => a,
        Err(_) => {
            // This is undesirable.  We stop now, which will cause the test to fail because the
            // crashtracking data won't get written (and the INVALID file still exists)
            return;
        }
    };

    // We're ready to call the handler.  Delete INVALID first.
    let filepath = match atom_to_clone(&OUTPUT_FILE) {
        Ok(f) => f,
        Err(_) => {
            // Fails the test, as above
            return;
        }
    };
    remove_file_permissive(&filepath);

    match old_action.handler() {
        SigHandler::SigDfl => {
            unsafe { signal::sigaction(signal::SIGSEGV, &old_action) }.ok();
        }
        SigHandler::Handler(fun) => fun(signum),
        SigHandler::SigAction(fun) => fun(signum, sig_info, ucontext),
        _ => (), // IGN, err, etc. -- all will fail
    }
}

fn test(output_dir: &str) -> anyhow::Result<()> {
    // First, check that OLD_ACTION is null
    if !OLD_ACTION.load(SeqCst).is_null() {
        anyhow::bail!("Test found invalid condition: OLD_ACTION was not null");
    }

    // Create the SigAction for the new handler
    let sig_action = SigAction::new(
        SigHandler::SigAction(segfault_sigaction),
        SaFlags::empty(),
        SigSet::empty(),
    );

    // Store the old handler, but if it doesn't exist that's an error
    let old_handler = unsafe { signal::sigaction(signal::SIGSEGV, &sig_action) }?;
    if old_handler.handler() == SigHandler::SigDfl || old_handler.handler() == SigHandler::SigIgn {
        anyhow::bail!("Test found invalid condition: old_handler was not a valid handler");
    }
    OLD_ACTION.store(Box::into_raw(Box::new(old_handler)), SeqCst);

    // At this point, we've successfully installed the handler. We create the INVALID file, which
    // will be deleted by the signal handler, as a preventative check in case the handler fails to
    // trigger in the first place.  Otherwise, this test will pass as long as the crashtracking
    // data is received correctly.
    set_atomic(&OUTPUT_FILE, format!("{output_dir}/INVALID"));
    let filepath = match atom_to_clone(&OUTPUT_FILE) {
        Ok(f) => f,
        Err(_) => {
            // Uh, if we can't read the value, then that's a problem
            anyhow::bail!("Error: OUTPUT_FILE was null");
        }
    };
    remove_file_permissive(&filepath);
    file_append_msg(&filepath, "File not cleared by handler")?;

    Ok(())
}
