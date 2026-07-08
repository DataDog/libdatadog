// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Signal disposition querying, installation, removal, and low-level signal helpers.

use core::ffi::{c_int, c_void};
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::crash::crash_handler;
use super::crash_debug;
use crate::collector_signal_safe::capabilities;
use crate::collector_signal_safe::config;
use crate::collector_signal_safe::policy::app_handler_is_real;
use crate::collector_signal_safe::state::{self, sig_index};

#[derive(Clone, Copy)]
pub(super) struct Target {
    pub(super) fn_ptr: *mut c_void,
    pub(super) flags: i32,
}

pub(super) fn effective_target(idx: usize) -> Target {
    let (fn_ptr, flags) = state::signal_slot(idx).original_handler();
    Target { fn_ptr, flags }
}

pub(super) fn query_sigaction(sig: c_int) -> Option<libc::sigaction> {
    let mut out: libc::sigaction = unsafe { core::mem::zeroed() };
    if unsafe { libc::sigaction(sig, null_mut(), &mut out) } == 0 {
        Some(out)
    } else {
        None
    }
}

fn is_our_handler(sig: c_int) -> bool {
    let Some(cur) = query_sigaction(sig) else {
        return false;
    };
    cur.sa_flags & libc::SA_SIGINFO != 0 && cur.sa_sigaction == crash_handler as *const () as usize
}

fn build_crash_sigaction() -> libc::sigaction {
    let mut sa: libc::sigaction = unsafe { core::mem::zeroed() };
    sa.sa_sigaction = crash_handler as *const () as usize;
    sa.sa_flags = libc::SA_SIGINFO;
    if state::SETTINGS.use_alt_stack.load(Ordering::Relaxed) {
        sa.sa_flags |= libc::SA_ONSTACK;
    }
    unsafe {
        libc::sigemptyset(&mut sa.sa_mask);
        if state::SETTINGS.block_signals.load(Ordering::Relaxed) {
            for &blocked in &config::CRASH_SIGNALS {
                let _ = libc::sigaddset(&mut sa.sa_mask, blocked);
            }
        }
    }
    sa
}

pub(super) fn reset_signals_to_default(signals: &[c_int]) -> bool {
    let mut dfl: libc::sigaction = unsafe { core::mem::zeroed() };
    dfl.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&mut dfl.sa_mask);
    }
    let mut ok = true;
    for &sig in signals {
        ok &= unsafe { libc::sigaction(sig, &dfl, null_mut()) == 0 };
    }
    ok
}

pub(super) unsafe fn unblock_signal(sig: c_int) {
    let mut set: libc::sigset_t = core::mem::zeroed();
    libc::sigemptyset(&mut set);
    libc::sigaddset(&mut set, sig);
    libc::sigprocmask(libc::SIG_UNBLOCK, &set, null_mut());
}

fn install_crash_handler(sig: c_int) {
    let Some(cur) = query_sigaction(sig) else {
        return;
    };
    if cur.sa_sigaction != libc::SIG_DFL {
        if app_handler_is_real(cur.sa_sigaction as *mut c_void) {
            if let Some(i) = sig_index(sig) {
                state::signal_slot(i).set_app_handler_present();
            }
            capabilities::note_degraded(capabilities::DEGRADED_APP_HANDLER_PRESENT);
            crash_debug(b"app handler present", sig);
        }
        return;
    }

    let sa = build_crash_sigaction();
    let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
    if unsafe { libc::sigaction(sig, &sa, &mut old) } != 0 {
        return;
    }

    if let Some(i) = sig_index(sig) {
        state::signal_slot(i).store_original_handler(
            old.sa_sigaction as *mut c_void,
            old.sa_flags,
            &old.sa_mask,
        );
        state::signal_slot(i).set_owned(true);
    }
}

fn uninstall_crash_handler(sig: c_int) {
    if !is_our_handler(sig) {
        return;
    }
    let Some(i) = sig_index(sig) else {
        return;
    };

    let target = effective_target(i);
    let mut restore: libc::sigaction = unsafe { core::mem::zeroed() };
    restore.sa_sigaction = target.fn_ptr as usize;
    restore.sa_flags = target.flags;
    unsafe {
        state::signal_slot(i).load_original_mask(&mut restore.sa_mask);
        if libc::sigaction(sig, &restore, null_mut()) == 0 {
            state::signal_slot(i).set_owned(false);
        }
    }
}

pub(super) fn install_all_handlers() {
    state::clear_signal_state();
    for &sig in &config::CRASH_SIGNALS {
        install_crash_handler(sig);
    }
}

pub(super) fn uninstall_all_handlers() {
    for &sig in &config::CRASH_SIGNALS {
        uninstall_crash_handler(sig);
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
pub(super) unsafe fn siginfo_pid(info: *mut libc::siginfo_t) -> i32 {
    (*info).si_pid()
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
pub(super) unsafe fn siginfo_pid(_info: *mut libc::siginfo_t) -> i32 {
    i32::MIN
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
pub(super) unsafe fn siginfo_addr(info: *mut libc::siginfo_t) -> usize {
    (*info).si_addr() as usize
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
pub(super) unsafe fn siginfo_addr(_info: *mut libc::siginfo_t) -> usize {
    0
}
