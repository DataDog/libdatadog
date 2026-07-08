// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;

use super::signal_names::{SI_ASYNCIO, SI_MESGQ, SI_QUEUE, SI_SIGIO, SI_TIMER, SI_TKILL, SI_USER};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Default,
    Ignore,
    Handler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainAction {
    RestoreDefaultAndRefault,
    RestoreDefaultAndReraise,
}

pub(super) fn disposition_of(handler: *mut c_void) -> Disposition {
    match handler as usize {
        value if value == libc::SIG_DFL => Disposition::Default,
        value if value == libc::SIG_IGN => Disposition::Ignore,
        _ => Disposition::Handler,
    }
}

pub(super) fn app_handler_is_real(handler: *mut c_void) -> bool {
    matches!(disposition_of(handler), Disposition::Handler)
}

pub(super) fn is_genuine_fault(
    has_siginfo: bool,
    si_code: i32,
    si_pid: i32,
    self_pid: i32,
) -> bool {
    if !has_siginfo {
        return false;
    }
    if si_code != SI_USER && si_code != SI_TKILL {
        return true;
    }
    si_pid == self_pid
}

pub(super) fn chain_action(
    disposition: Disposition,
    has_siginfo: bool,
    si_code: i32,
) -> ChainAction {
    // Owned signal slots are installed only over SIG_DFL today. If a non-default disposition ever
    // appears here before app-handler chaining is implemented, keep terminal crash semantics
    // instead of invoking unreachable app code.
    match (disposition, should_refault(has_siginfo, si_code)) {
        (_, true) => ChainAction::RestoreDefaultAndRefault,
        (Disposition::Default | Disposition::Ignore | Disposition::Handler, false) => {
            ChainAction::RestoreDefaultAndReraise
        }
    }
}

fn should_refault(has_siginfo: bool, si_code: i32) -> bool {
    has_siginfo && si_code > 0 && !is_async_si_code(si_code)
}

fn is_async_si_code(si_code: i32) -> bool {
    matches!(
        si_code,
        SI_USER | SI_QUEUE | SI_TIMER | SI_MESGQ | SI_ASYNCIO | SI_SIGIO | SI_TKILL
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector_signal_safe::signal_names::{SEGV_MAPERR, SI_USER};

    #[test]
    fn dispositions_match_sigaction_sentinels() {
        let dfl = libc::SIG_DFL as *mut c_void;
        let ign = libc::SIG_IGN as *mut c_void;
        let handler = 0x1234usize as *mut c_void;

        assert_eq!(disposition_of(dfl), Disposition::Default);
        assert_eq!(disposition_of(core::ptr::null_mut()), Disposition::Default);
        assert_eq!(disposition_of(ign), Disposition::Ignore);
        assert_eq!(disposition_of(handler), Disposition::Handler);
        assert!(!app_handler_is_real(dfl));
        assert!(!app_handler_is_real(ign));
        assert!(app_handler_is_real(handler));
    }

    #[test]
    fn genuine_fault_filter_ignores_external_async_signal() {
        assert!(!is_genuine_fault(true, SI_USER, 7, 9));
    }

    #[test]
    fn genuine_fault_filter_accepts_self_sent_async_signal() {
        assert!(is_genuine_fault(true, SI_USER, 9, 9));
    }

    #[test]
    fn chain_action_matches_default_signal_semantics() {
        assert_eq!(
            chain_action(Disposition::Default, true, SEGV_MAPERR),
            ChainAction::RestoreDefaultAndRefault
        );
        assert_eq!(
            chain_action(Disposition::Default, true, SI_USER),
            ChainAction::RestoreDefaultAndReraise
        );
        assert_eq!(
            chain_action(Disposition::Handler, true, SEGV_MAPERR),
            ChainAction::RestoreDefaultAndRefault
        );
        assert_eq!(
            chain_action(Disposition::Ignore, true, SI_USER),
            ChainAction::RestoreDefaultAndReraise
        );
    }
}
