// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;

use super::signal_names::{SI_TKILL, SI_USER};

const SIG_DFL_VALUE: usize = 0;
const SIG_IGN_VALUE: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Default,
    Ignore,
    Handler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainAction {
    InvokeApp,
    RestoreDefaultAndRefault,
    RestoreDefaultAndReraise,
    Resume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalContext {
    pub has_siginfo: bool,
    pub si_code: i32,
    pub si_pid: i32,
    pub self_pid: i32,
}

impl SignalContext {
    pub fn is_genuine_fault(self) -> bool {
        is_genuine_fault(self.has_siginfo, self.si_code, self.si_pid, self.self_pid)
    }
}

pub fn disposition_of(handler: *mut c_void) -> Disposition {
    match handler as usize {
        SIG_DFL_VALUE => Disposition::Default,
        SIG_IGN_VALUE => Disposition::Ignore,
        _ => Disposition::Handler,
    }
}

pub fn app_handler_is_real(handler: *mut c_void) -> bool {
    matches!(disposition_of(handler), Disposition::Handler)
}

pub fn should_run_app_first(force_on_top: bool, app_is_real: bool) -> bool {
    !force_on_top && app_is_real
}

pub fn app_recovered(handler_after: *mut c_void) -> bool {
    disposition_of(handler_after) != Disposition::Default
}

pub fn is_genuine_fault(has_siginfo: bool, si_code: i32, si_pid: i32, self_pid: i32) -> bool {
    if !has_siginfo {
        return false;
    }
    if si_code != SI_USER && si_code != SI_TKILL {
        return true;
    }
    si_pid == self_pid
}

pub fn chain_action(disposition: Disposition, has_siginfo: bool, si_code: i32) -> ChainAction {
    match disposition {
        Disposition::Ignore => ChainAction::Resume,
        Disposition::Handler => ChainAction::InvokeApp,
        Disposition::Default if has_siginfo && si_code > 0 => ChainAction::RestoreDefaultAndRefault,
        Disposition::Default => ChainAction::RestoreDefaultAndReraise,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector_signal_safe::signal_names::{SEGV_MAPERR, SI_USER};

    #[test]
    fn dispositions_match_sigaction_sentinels() {
        let dfl = SIG_DFL_VALUE as *mut c_void;
        let ign = SIG_IGN_VALUE as *mut c_void;
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
    fn handler_policy_tracks_application_recovery() {
        let dfl = SIG_DFL_VALUE as *mut c_void;
        let ign = SIG_IGN_VALUE as *mut c_void;
        let handler = 0x1234usize as *mut c_void;

        assert!(should_run_app_first(false, true));
        assert!(!should_run_app_first(true, true));
        assert!(!should_run_app_first(false, false));

        assert!(app_recovered(handler));
        assert!(app_recovered(ign));
        assert!(!app_recovered(dfl));
    }

    #[test]
    fn disposition_based_chain_action_resumes_ignored_signals() {
        assert_eq!(
            chain_action(Disposition::Ignore, true, SEGV_MAPERR),
            ChainAction::Resume
        );
    }

    #[test]
    fn genuine_fault_filter_ignores_external_async_signal() {
        let ctx = SignalContext {
            has_siginfo: true,
            si_code: SI_USER,
            si_pid: 7,
            self_pid: 9,
        };

        assert!(!ctx.is_genuine_fault());
    }

    #[test]
    fn genuine_fault_filter_accepts_self_sent_async_signal() {
        let ctx = SignalContext {
            has_siginfo: true,
            si_code: SI_USER,
            si_pid: 9,
            self_pid: 9,
        };

        assert!(ctx.is_genuine_fault());
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
            ChainAction::InvokeApp
        );
    }
}
