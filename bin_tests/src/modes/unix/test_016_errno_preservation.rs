// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Checks that crashtracker's signal handler preserves errno across its
// execution. The handler saves errno on entry and restores it before chaining,
// so a chained handler should see the same errno that was current when the
// signal fired.
//
// Expected operation
// 1. post() sets errno to EXPECTED_ERRNO (42)
// 2. SIGSEGV is triggered
// 3. crashtracker handles the crash, then restores errno and chains this test's SIGSEGV handler
// 4. this test's SIGSEGV handler reads errno, writes either "PRESERVED" or "MISMATCHED" to
//    ERRNO_STATUS_FILENAME, then raises SIGABRT to exit
//
//  The integration test reads ERRNO_STATUS_FILENAME and asserts "PRESERVED"
use crate::modes::behavior::Behavior;

use errno::{errno, set_errno, Errno};
use libc;
use libdd_crashtracker::CrashtrackerConfiguration;
use nix::{
    sys::signal::{self, kill, SaFlags, SigAction, SigHandler, SigSet, Signal},
    unistd::Pid,
};
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

/// The errno value we set before the crash and expect to see in the chained handler.
const EXPECTED_ERRNO: i32 = 42;

pub const ERRNO_STATUS_FILENAME: &str = "errno_status";
static STATUS_PATH: AtomicPtr<CString> = AtomicPtr::new(std::ptr::null_mut());

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        let path = output_dir.join(ERRNO_STATUS_FILENAME);
        let cpath = CString::new(path.as_os_str().as_encoded_bytes())?;
        crate::modes::behavior::set_atomic(&STATUS_PATH, cpath);
        setup()
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Set errno to known value right before crash
        set_errno(Errno(EXPECTED_ERRNO));
        Ok(())
    }
}

extern "C" fn segv_sigaction(
    _signum: i32,
    _sig_info: *mut libc::siginfo_t,
    _ucontext: *mut libc::c_void,
) {
    let actual_errno = errno().0;

    let path_ptr = STATUS_PATH.load(Ordering::SeqCst);
    if !path_ptr.is_null() {
        let cpath = unsafe { &*path_ptr };
        // open/write/close are async signal safe
        unsafe {
            let fd = libc::open(
                cpath.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o644,
            );
            if fd >= 0 {
                let msg = if actual_errno == EXPECTED_ERRNO {
                    b"PRESERVED" as &[u8]
                } else {
                    b"MISMATCHED"
                };
                libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
                libc::close(fd);
            }
        }
    }

    let _ = kill(Pid::this(), Signal::SIGABRT);
}

extern "C" fn abort_sigaction(
    _signum: i32,
    _sig_info: *mut libc::siginfo_t,
    _ucontext: *mut libc::c_void,
) {
    unsafe {
        libc::_exit(128 + _signum);
    }
}

pub fn setup() -> anyhow::Result<()> {
    let sig_action = SigAction::new(
        SigHandler::SigAction(segv_sigaction),
        SaFlags::empty(),
        SigSet::empty(),
    );
    let _ = unsafe { signal::sigaction(signal::SIGSEGV, &sig_action) }?;

    let sig_action = SigAction::new(
        SigHandler::SigAction(abort_sigaction),
        SaFlags::empty(),
        SigSet::empty(),
    );
    let _ = unsafe { signal::sigaction(signal::SIGABRT, &sig_action) }?;
    Ok(())
}
