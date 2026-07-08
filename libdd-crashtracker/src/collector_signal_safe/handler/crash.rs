// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crash-handler entry point and final default-signal dispatch.

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

use super::collect::{collect_crash, CrashEvent};
use super::sigaction::{effective_target, reset_signals_to_default, unblock_signal, Target};
use super::{crash_debug, EXIT_CODE_FAILURE};
use crate::collector_signal_safe::policy::{
    chain_action, disposition_of, is_genuine_fault, ChainAction,
};
use crate::collector_signal_safe::state::{self, sig_index};
use crate::collector_signal_safe::sys;

/// Prevents recursive crash collection. Reset only during explicit shutdown/re-init lifecycle.
static COLLECTING: AtomicBool = AtomicBool::new(false);

pub(super) fn reset_collecting() {
    COLLECTING.store(false, Ordering::Relaxed);
}

pub(super) extern "C" fn crash_handler(
    sig: c_int,
    info: *mut libc::siginfo_t,
    ucontext: *mut c_void,
) {
    if !state::HANDLERS_ENABLED.load(Ordering::Acquire) {
        return;
    }

    let saved_errno = sys::errno();
    crash_debug(b"handler entered", sig);
    if state::SETTINGS.disarm_on_entry.load(Ordering::Relaxed) {
        let _ = reset_signals_to_default(&[sig]);
    }

    let idx = sig_index(sig);
    let event = CrashEvent::from_signal(sig, info, ucontext);

    let target = match idx {
        Some(i) => effective_target(i),
        None => Target {
            fn_ptr: core::ptr::null_mut(),
            flags: 0,
        },
    };

    let genuine_fault = is_genuine_fault(event.has_info, event.si_code, event.si_pid, event.pid);
    if genuine_fault && !COLLECTING.swap(true, Ordering::Relaxed) {
        collect_crash(event);
    }

    sys::set_errno(saved_errno);

    let action = chain_action(disposition_of(target.fn_ptr), event.has_info, event.si_code);
    match action {
        ChainAction::RestoreDefaultAndRefault | ChainAction::RestoreDefaultAndReraise => {
            if !reset_signals_to_default(&[sig]) {
                sys::exit_process(EXIT_CODE_FAILURE);
            }
            unsafe {
                if let ChainAction::RestoreDefaultAndReraise = action {
                    unblock_signal(sig);
                    libc::raise(sig);
                    sys::exit_process(EXIT_CODE_FAILURE);
                }
            }
        }
    }
}
