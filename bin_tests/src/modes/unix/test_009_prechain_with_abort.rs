// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// * registers SIGABRT and SIGSEGV handlers before crashtracker gets installed
// * the SIGSEGV handler raises SIGABRT
// * the SIGABRT handler passes the test exits the process gracefully
// * this is intended to catch issues with signal chaining, mostly deadlocks
//
// Expected operation
// 1. SIGSEGV is thrown
// 2. crashtracker handles the crash and chains this test's handler
// 3. this test's SIGSEGV handler calls SIGABRT
// 4. *either* crashtracker is invoked and chains this test's SIGABRT handler, *or* crashtracker
//    uninstalls itself in part 2 and this handler is called correctly
//
//  Basically, this test fails if this test's SIGABRT handler fails to trigger
use crate::modes::behavior::Behavior;

use datadog_crashtracker::CrashtrackerConfiguration;
use libc;
use nix::{
    sys::signal::{self, kill, SaFlags, SigAction, SigHandler, SigSet, Signal},
    unistd::Pid,
};
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        setup()
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

extern "C" fn segv_sigaction(
    _signum: i32,
    _sig_info: *mut libc::siginfo_t,
    _ucontext: *mut libc::c_void,
) {
    let _ = kill(Pid::this(), Signal::SIGABRT);
}

extern "C" fn abort_sigaction(
    _signum: i32,
    _sig_info: *mut libc::siginfo_t,
    _ucontext: *mut libc::c_void,
) {
    // Exit exactly as though this were a segfault
    unsafe {
        libc::_exit(128 + _signum);
    }
}

pub fn setup() -> anyhow::Result<()> {
    // Emulate someone's application--don't try to chain or store anything
    let sig_action = SigAction::new(
        SigHandler::SigAction(segv_sigaction),
        SaFlags::empty(),
        SigSet::empty(),
    );
    let _ = unsafe { signal::sigaction(signal::SIGSEGV, &sig_action) }?;

    // And sigabrt
    let sig_action = SigAction::new(
        SigHandler::SigAction(abort_sigaction),
        SaFlags::empty(),
        SigSet::empty(),
    );
    let _ = unsafe { signal::sigaction(signal::SIGABRT, &sig_action) }?;
    Ok(())
}
