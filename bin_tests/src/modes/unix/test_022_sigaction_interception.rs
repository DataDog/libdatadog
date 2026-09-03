// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Verifies that the sigaction GOT hook fires when the test binary installs a
// SIGSEGV handler after crashtracker init.
//
// Because hook_symbol (not hook_symbol_excluding_self) is used, the test
// binary's GOT is patched at init time. Calling signal::sigaction from post()
// therefore goes through hook_sigaction, which emits a telemetry warning.
//
// The installed handler chains back to the old one (crashtracker's), so the
// crash report is still generated and the test can validate both outcomes:
//   1. Crash report generated correctly.
//   2. Telemetry file contains the `sigaction_intercepted` warning.
use crate::modes::behavior::{atom_to_clone, set_atomic};
use libc;
use libdd_crashtracker::CrashtrackerConfiguration;
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use std::path::Path;
use std::sync::atomic::AtomicPtr;

pub struct Test;

impl crate::modes::behavior::Behavior for Test {
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
        install_handler()
    }
}

/// Stores the handler that was active before we installed ours (crashtracker's).
static OLD_ACTION: AtomicPtr<SigAction> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn sigsegv_handler(
    signum: i32,
    sig_info: *mut libc::siginfo_t,
    ucontext: *mut libc::c_void,
) {
    // Chain to the old handler so crashtracker receives the signal and
    // generates a crash report.
    let old = match atom_to_clone(&OLD_ACTION) {
        Ok(a) => a,
        Err(_) => return,
    };
    match old.handler() {
        SigHandler::SigDfl => {
            unsafe { signal::sigaction(signal::SIGSEGV, &old) }.ok();
        }
        SigHandler::SigIgn => {}
        SigHandler::Handler(f) => f(signum),
        SigHandler::SigAction(f) => f(signum, sig_info, ucontext),
    }
}

fn install_handler() -> anyhow::Result<()> {
    let sig_action = SigAction::new(
        SigHandler::SigAction(sigsegv_handler),
        SaFlags::empty(),
        SigSet::empty(),
    );

    // This call goes through the test binary's patched GOT (hook_symbol patches
    // all libraries including ours), so hook_sigaction fires and emits telemetry.
    // The returned old_handler is the crashtracker's handle_posix_sigaction.
    let old_handler = unsafe { signal::sigaction(signal::SIGSEGV, &sig_action) }?;
    set_atomic(&OLD_ACTION, old_handler);

    // Give the background telemetry thread time to write the warning to the
    // telemetry file before the crash kills the process.
    std::thread::sleep(std::time::Duration::from_millis(300));

    Ok(())
}
