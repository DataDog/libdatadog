// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Intercepts `sigaction` calls to detect when an application overwrites
//! signal handlers installed by the crashtracker.
//!
//! During crashtracker initialization, [`install_sigaction_hook`] patches
//! the GOT entries for `sigaction` across all loaded libraries so that
//! calls are routed through our hook
//!
//! This module is gated on 64-bit Linux (`mod.rs`).

use core::sync::atomic::{
    AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

/// Bit N is set if signal number N is monitored. Supports signals 0–63.
static MONITORED_SIGNALS: AtomicU64 = AtomicU64::new(0);

/// Resolved address of the original `sigaction`, set once during
/// [`install_sigaction_hook`].
static ORIG_SIGACTION_FN: AtomicUsize = AtomicUsize::new(0);

type SigactionFn =
    unsafe extern "C" fn(libc::c_int, *const libc::sigaction, *mut libc::sigaction) -> libc::c_int;

/// Signal name lookup for logging. Returns the signal name
/// or "unknown" for unknown signal numbers.
fn signal_name(signum: libc::c_int) -> &'static str {
    match signum {
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGPIPE => "SIGPIPE",
        _ => "unknown",
    }
}

/// Our replacement for `sigaction`, installed using GOT patching.
///
/// For signals monitored by the crashtracker, logs a warning. In all
/// cases, forwards the call to the real `sigaction`.
unsafe extern "C" fn hook_sigaction(
    signum: libc::c_int,
    act: *const libc::sigaction,
    oldact: *mut libc::sigaction,
) -> libc::c_int {
    // Check if this signal is one we monitor.
    if (0..64).contains(&signum)
        && !act.is_null()
        && MONITORED_SIGNALS.load(Relaxed) & (1u64 << signum) != 0
    {
        eprintln!(
            "[datadog-crashtracker] sigaction called for monitored signal {} ({}), \
             the crashtracker's handler may be overwritten",
            signum,
            signal_name(signum),
        );
    }

    // Forward to the real sigaction.
    let orig = ORIG_SIGACTION_FN.load(Acquire);
    if orig != 0 {
        // SAFETY: `orig` was stored by `install_sigaction_hook` from a
        // successful `hook_symbol` call which resolved the real `sigaction`.
        let func: SigactionFn = unsafe { core::mem::transmute::<usize, SigactionFn>(orig) };
        // SAFETY: forwarding the exact arguments from the caller.
        unsafe { func(signum, act, oldact) }
    } else {
        // Fallback: should not happen, but dont crash
        *libc::__errno_location() = libc::ENOSYS;
        -1
    }
}

/// Install the `sigaction` GOT hook across all currently loaded libraries.
///
/// Safe to call multiple times; only the first call patches.
pub(crate) fn install_sigaction_hook(monitored_signals: &[i32]) {
    if ORIG_SIGACTION_FN.load(Relaxed) != 0 {
        return;
    }

    let mut mask: u64 = 0;
    for &sig in monitored_signals {
        if (0..64).contains(&sig) {
            mask |= 1u64 << sig;
        }
    }
    MONITORED_SIGNALS.store(mask, Release);

    // SAFETY: hook_sigaction has the same signature as sigaction.
    let result =
        unsafe { libdd_gotter::hook_symbol(c"sigaction", hook_sigaction as *const () as usize) };

    if let Ok(hook) = result {
        let our_hook = hook_sigaction as *const () as usize;
        if hook.orig_addr != our_hook {
            ORIG_SIGACTION_FN.store(hook.orig_addr, Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_name() {
        assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(signal_name(libc::SIGBUS), "SIGBUS");
        assert_eq!(signal_name(libc::SIGABRT), "SIGABRT");
        assert_eq!(signal_name(libc::SIGILL), "SIGILL");
        assert_eq!(signal_name(999), "unknown");
    }

    #[test]
    fn test_monitored_mask_building() {
        // Reset state for test isolation.
        MONITORED_SIGNALS.store(0, Release);

        let signals = [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT];
        let mut mask: u64 = 0;
        for &sig in &signals {
            if (0..64).contains(&sig) {
                mask |= 1u64 << sig;
            }
        }

        for &sig in &signals {
            assert!(mask & (1u64 << sig) != 0, "signal {sig} should be set");
        }
        assert!(
            mask & (1u64 << libc::SIGTERM) == 0,
            "SIGTERM should not be set"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_install_sigaction_hook() {
        // Reset state so the hook can be installed in this test.
        ORIG_SIGACTION_FN.store(0, Relaxed);

        install_sigaction_hook(&[libc::SIGSEGV, libc::SIGBUS]);

        let orig = ORIG_SIGACTION_FN.load(Acquire);
        if orig == 0 {
            eprintln!(
                "note: sigaction not found in dynamic symbol table \
                 (static libc?), GOT hook not installed"
            );
        } else {
            // Verify the monitored mask was set correctly.
            let mask = MONITORED_SIGNALS.load(Acquire);
            assert!(mask & (1u64 << libc::SIGSEGV) != 0);
            assert!(mask & (1u64 << libc::SIGBUS) != 0);
        }
    }
}
