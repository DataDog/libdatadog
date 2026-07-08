// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crash-handler entry point, app-handler chaining guard, and final chain dispatch.

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use super::collect::{collect_crash, CrashEvent};
use super::sigaction::{
    effective_target, invoke_handler, query_sigaction, reset_signals_to_default,
    restore_our_handler, unblock_signal, Target,
};
use super::{crash_debug, EXIT_CODE_FAILURE};
use crate::collector_signal_safe::policy::{
    app_handler_is_real, app_recovered, chain_action, disposition_of, is_genuine_fault,
    should_run_app_first, ChainAction,
};
use crate::collector_signal_safe::state::{self, sig_index};
use crate::collector_signal_safe::sys;

static APP_CHAIN_TID: AtomicI32 = AtomicI32::new(0);
static APP_CHAIN_STACK: AtomicUsize = AtomicUsize::new(0);

struct RepeatFaultSlot {
    pc: AtomicUsize,
    addr: AtomicUsize,
    count: AtomicUsize,
}

impl RepeatFaultSlot {
    const fn new() -> Self {
        Self {
            pc: AtomicUsize::new(0),
            addr: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
        }
    }

    fn tripped(&self, pc: usize, addr: usize) -> bool {
        if pc == 0 {
            return false;
        }

        let last_pc = self.pc.load(Ordering::Relaxed);
        let last_addr = self.addr.load(Ordering::Relaxed);
        if last_pc == pc && last_addr == addr {
            self.count.fetch_add(1, Ordering::Relaxed) + 1 >= 2
        } else {
            self.addr.store(addr, Ordering::Relaxed);
            self.count.store(1, Ordering::Relaxed);
            self.pc.store(pc, Ordering::Relaxed);
            false
        }
    }

    #[cfg(test)]
    fn reset(&self) {
        self.pc.store(0, Ordering::Relaxed);
        self.addr.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }
}

static REPEAT_FAULT: [RepeatFaultSlot; state::NSIG] =
    [const { RepeatFaultSlot::new() }; state::NSIG];
/// Prevents recursive crash collection. Reset only during explicit shutdown/re-init lifecycle.
static COLLECTING: AtomicBool = AtomicBool::new(false);

pub(super) fn reset_collecting() {
    COLLECTING.store(false, Ordering::Relaxed);
}

/// Tracks app-first handler invocation without relying on cleanup after the call.
///
/// A recovering app handler may leave this frame via siglongjmp, so a simple boolean would stay
/// set forever. Supported Unix targets use downward-growing stacks: a nested crash inside the app
/// handler has a stack address below the recorded frame, while a later signal after longjmp has
/// unwound above it. Different-thread entries skip app-first while the earlier handler is active.
fn enter_app_chain(tid: i32, stack_pos: usize) -> bool {
    let owner = APP_CHAIN_TID.load(Ordering::Relaxed);
    if owner == 0 {
        APP_CHAIN_STACK.store(stack_pos, Ordering::Relaxed);
        APP_CHAIN_TID.store(tid, Ordering::Relaxed);
        return true;
    }

    if owner != tid {
        return false;
    }

    let recorded = APP_CHAIN_STACK.load(Ordering::Relaxed);
    if stack_pos > recorded {
        APP_CHAIN_STACK.store(stack_pos, Ordering::Relaxed);
        APP_CHAIN_TID.store(tid, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn leave_app_chain(tid: i32, stack_pos: usize) {
    if APP_CHAIN_TID.load(Ordering::Relaxed) == tid
        && APP_CHAIN_STACK.load(Ordering::Relaxed) == stack_pos
    {
        APP_CHAIN_STACK.store(0, Ordering::Relaxed);
        APP_CHAIN_TID.store(0, Ordering::Relaxed);
    }
}

fn app_return_repeated_fault(idx: usize, pc: usize, addr: usize) -> bool {
    REPEAT_FAULT[idx].tripped(pc, addr)
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
    let disarmed_on_entry =
        state::SETTINGS.disarm_on_entry.load(Ordering::Relaxed) && reset_signals_to_default(&[sig]);

    let idx = sig_index(sig);
    let event = CrashEvent::from_signal(sig, info, ucontext);

    // The installed target is immutable while the handler runs, so resolve it once and reuse it for
    // both the app-first chain and the final chaining decision below.
    let target = match idx {
        Some(i) => effective_target(i),
        None => Target {
            fn_ptr: core::ptr::null_mut(),
            flags: 0,
        },
    };

    let force_on_top = state::SETTINGS.force_on_top.load(Ordering::Relaxed);
    if let Some(i) = idx {
        let app_is_real = app_handler_is_real(target.fn_ptr);
        if should_run_app_first(force_on_top, app_is_real) {
            let stack_marker = 0u8;
            let stack_pos = (&stack_marker as *const u8) as usize;
            if enter_app_chain(event.tid, stack_pos) {
                sys::set_errno(saved_errno);
                // If the application handler recovers with siglongjmp, no code after this call
                // runs. Keep this path free of Drop-dependent state.
                unsafe { invoke_handler(&target, sig, info, ucontext) };

                let handler_after = live_handler_for_recovery(sig).unwrap_or(target.fn_ptr);
                leave_app_chain(event.tid, stack_pos);
                if app_recovered(handler_after) {
                    let pc = event.instruction_pointer();
                    if app_return_repeated_fault(i, pc, event.si_addr) {
                        crash_debug(b"app handler returned without recovery", sig);
                    } else {
                        if disarmed_on_entry {
                            restore_our_handler(sig);
                        }
                        sys::set_errno(saved_errno);
                        return;
                    }
                }
            } else {
                crash_debug(b"app handler recursion detected", sig);
            }
        }
    }

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
        ChainAction::Resume => {
            if disarmed_on_entry {
                restore_our_handler(sig);
            }
        }
        ChainAction::InvokeApp => unsafe {
            if disarmed_on_entry && !genuine_fault {
                restore_our_handler(sig);
            }
            invoke_handler(&target, sig, info, ucontext);
        },
    }
}

fn live_handler_for_recovery(sig: c_int) -> Option<*mut c_void> {
    query_sigaction(sig).map(|cur| cur.sa_sigaction as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_chain_guard_distinguishes_recursion_from_unwind() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        APP_CHAIN_TID.store(0, Ordering::Relaxed);
        APP_CHAIN_STACK.store(0, Ordering::Relaxed);

        assert!(enter_app_chain(123, 1_000));
        assert!(!enter_app_chain(123, 900));
        assert!(!enter_app_chain(456, 1_100));
        assert!(enter_app_chain(123, 1_100));
        leave_app_chain(123, 1_100);
        assert_eq!(APP_CHAIN_TID.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn repeated_app_return_trips_on_second_same_fault() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");
        let idx = sig_index(libc::SIGSEGV).expect("SIGSEGV index");

        REPEAT_FAULT[idx].reset();

        assert!(!app_return_repeated_fault(idx, 0x1234, 0));
        assert!(app_return_repeated_fault(idx, 0x1234, 0));
        assert!(!app_return_repeated_fault(idx, 0x5678, 0));
    }
}
