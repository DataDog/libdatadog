// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SigInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub si_addr: Option<String>,
    pub si_code: libc::c_int,
    pub si_code_human_readable: SiCodes,
    pub si_signo: libc::c_int,
    pub si_signo_human_readable: SignalNames,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
#[repr(C)]
/// See https://man7.org/linux/man-pages/man7/signal.7.html
pub enum SignalNames {
    SIGHUP,
    SIGINT,
    SIGQUIT,
    SIGILL,
    SIGTRAP,
    SIGABRT,
    SIGBUS,
    SIGFPE,
    SIGKILL,
    SIGUSR1,
    SIGSEGV,
    SIGUSR2,
    SIGPIPE,
    SIGALRM,
    SIGTERM,
    SIGCHLD,
    SIGCONT,
    SIGSTOP,
    SIGTSTP,
    SIGTTIN,
    SIGTTOU,
    SIGURG,
    SIGXCPU,
    SIGXFSZ,
    SIGVTALRM,
    SIGPROF,
    SIGWINCH,
    SIGIO,
    SIGSYS,
    SIGEMT,
    SIGINFO,
    UNKNOWN,
}

impl SignalNames {
    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "SIGHUP" => Self::SIGHUP,
            "SIGINT" => Self::SIGINT,
            "SIGQUIT" => Self::SIGQUIT,
            "SIGILL" => Self::SIGILL,
            "SIGTRAP" => Self::SIGTRAP,
            "SIGABRT" => Self::SIGABRT,
            "SIGBUS" => Self::SIGBUS,
            "SIGFPE" => Self::SIGFPE,
            "SIGKILL" => Self::SIGKILL,
            "SIGUSR1" => Self::SIGUSR1,
            "SIGSEGV" => Self::SIGSEGV,
            "SIGUSR2" => Self::SIGUSR2,
            "SIGPIPE" => Self::SIGPIPE,
            "SIGALRM" => Self::SIGALRM,
            "SIGTERM" => Self::SIGTERM,
            "SIGCHLD" => Self::SIGCHLD,
            "SIGCONT" => Self::SIGCONT,
            "SIGSTOP" => Self::SIGSTOP,
            "SIGTSTP" => Self::SIGTSTP,
            "SIGTTIN" => Self::SIGTTIN,
            "SIGTTOU" => Self::SIGTTOU,
            "SIGURG" => Self::SIGURG,
            "SIGXCPU" => Self::SIGXCPU,
            "SIGXFSZ" => Self::SIGXFSZ,
            "SIGVTALRM" => Self::SIGVTALRM,
            "SIGPROF" => Self::SIGPROF,
            "SIGWINCH" => Self::SIGWINCH,
            "SIGIO" => Self::SIGIO,
            "SIGSYS" => Self::SIGSYS,
            "SIGEMT" => Self::SIGEMT,
            "SIGINFO" => Self::SIGINFO,
            _ => Self::UNKNOWN,
        }
    }
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use super::*;

    impl From<libc::c_int> for SignalNames {
        fn from(value: libc::c_int) -> Self {
            SignalNames::from_name(crate::shared::signal_names::rust_signal_name(value))
        }
    }

    pub fn translate_si_code(signum: libc::c_int, si_code: libc::c_int) -> SiCodes {
        SiCodes::from_name(crate::shared::signal_names::rust_si_code_name(
            signum, si_code,
        ))
    }

    #[cfg(test)]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_si_code() {
        assert_eq!(
            translate_si_code(libc::SIGSEGV, crate::shared::signal_names::SEGV_ACCERR),
            SiCodes::SEGV_ACCERR
        );

        // An invalid code should translate to UNKNOWN
        assert_eq!(translate_si_code(libc::SIGSEGV, 42), SiCodes::UNKNOWN);
    }

    impl From<nix::sys::signal::Signal> for SignalNames {
        fn from(value: nix::sys::signal::Signal) -> Self {
            match value {
                nix::sys::signal::Signal::SIGHUP => SignalNames::SIGHUP,
                nix::sys::signal::Signal::SIGINT => SignalNames::SIGINT,
                nix::sys::signal::Signal::SIGQUIT => SignalNames::SIGQUIT,
                nix::sys::signal::Signal::SIGILL => SignalNames::SIGILL,
                nix::sys::signal::Signal::SIGTRAP => SignalNames::SIGTRAP,
                nix::sys::signal::Signal::SIGABRT => SignalNames::SIGABRT,
                nix::sys::signal::Signal::SIGBUS => SignalNames::SIGBUS,
                nix::sys::signal::Signal::SIGFPE => SignalNames::SIGFPE,
                nix::sys::signal::Signal::SIGKILL => SignalNames::SIGKILL,
                nix::sys::signal::Signal::SIGUSR1 => SignalNames::SIGUSR1,
                nix::sys::signal::Signal::SIGSEGV => SignalNames::SIGSEGV,
                nix::sys::signal::Signal::SIGUSR2 => SignalNames::SIGUSR2,
                nix::sys::signal::Signal::SIGPIPE => SignalNames::SIGPIPE,
                nix::sys::signal::Signal::SIGALRM => SignalNames::SIGALRM,
                nix::sys::signal::Signal::SIGTERM => SignalNames::SIGTERM,
                nix::sys::signal::Signal::SIGCHLD => SignalNames::SIGCHLD,
                nix::sys::signal::Signal::SIGCONT => SignalNames::SIGCONT,
                nix::sys::signal::Signal::SIGSTOP => SignalNames::SIGSTOP,
                nix::sys::signal::Signal::SIGTSTP => SignalNames::SIGTSTP,
                nix::sys::signal::Signal::SIGTTIN => SignalNames::SIGTTIN,
                nix::sys::signal::Signal::SIGTTOU => SignalNames::SIGTTOU,
                nix::sys::signal::Signal::SIGURG => SignalNames::SIGURG,
                nix::sys::signal::Signal::SIGXCPU => SignalNames::SIGXCPU,
                nix::sys::signal::Signal::SIGXFSZ => SignalNames::SIGXFSZ,
                nix::sys::signal::Signal::SIGVTALRM => SignalNames::SIGVTALRM,
                nix::sys::signal::Signal::SIGPROF => SignalNames::SIGPROF,
                nix::sys::signal::Signal::SIGWINCH => SignalNames::SIGWINCH,
                nix::sys::signal::Signal::SIGIO => SignalNames::SIGIO,
                nix::sys::signal::Signal::SIGSYS => SignalNames::SIGSYS,
                #[cfg(not(any(
                    target_os = "android",
                    target_os = "emscripten",
                    target_os = "fuchsia",
                    target_os = "linux",
                    target_os = "redox",
                    target_os = "haiku"
                )))]
                nix::sys::signal::Signal::SIGEMT => SignalNames::SIGEMT,
                #[cfg(not(any(
                    target_os = "android",
                    target_os = "emscripten",
                    target_os = "fuchsia",
                    target_os = "linux",
                    target_os = "redox",
                    target_os = "haiku",
                    target_os = "aix"
                )))]
                nix::sys::signal::Signal::SIGINFO => SignalNames::SIGINFO,
                _ => SignalNames::UNKNOWN,
            }
        }
    }

    /// Converts a signum into a Signal.  Can't use the from trait because we don't own either type.
    pub fn signal_from_signum(value: libc::c_int) -> anyhow::Result<nix::sys::signal::Signal> {
        let rval = match value {
            libc::SIGHUP => nix::sys::signal::Signal::SIGHUP,
            libc::SIGINT => nix::sys::signal::Signal::SIGINT,
            libc::SIGQUIT => nix::sys::signal::Signal::SIGQUIT,
            libc::SIGILL => nix::sys::signal::Signal::SIGILL,
            libc::SIGTRAP => nix::sys::signal::Signal::SIGTRAP,
            libc::SIGABRT => nix::sys::signal::Signal::SIGABRT,
            libc::SIGBUS => nix::sys::signal::Signal::SIGBUS,
            libc::SIGFPE => nix::sys::signal::Signal::SIGFPE,
            libc::SIGKILL => nix::sys::signal::Signal::SIGKILL,
            libc::SIGUSR1 => nix::sys::signal::Signal::SIGUSR1,
            libc::SIGSEGV => nix::sys::signal::Signal::SIGSEGV,
            libc::SIGUSR2 => nix::sys::signal::Signal::SIGUSR2,
            libc::SIGPIPE => nix::sys::signal::Signal::SIGPIPE,
            libc::SIGALRM => nix::sys::signal::Signal::SIGALRM,
            libc::SIGTERM => nix::sys::signal::Signal::SIGTERM,
            libc::SIGCHLD => nix::sys::signal::Signal::SIGCHLD,
            libc::SIGCONT => nix::sys::signal::Signal::SIGCONT,
            libc::SIGSTOP => nix::sys::signal::Signal::SIGSTOP,
            libc::SIGTSTP => nix::sys::signal::Signal::SIGTSTP,
            libc::SIGTTIN => nix::sys::signal::Signal::SIGTTIN,
            libc::SIGTTOU => nix::sys::signal::Signal::SIGTTOU,
            libc::SIGURG => nix::sys::signal::Signal::SIGURG,
            libc::SIGXCPU => nix::sys::signal::Signal::SIGXCPU,
            libc::SIGXFSZ => nix::sys::signal::Signal::SIGXFSZ,
            libc::SIGVTALRM => nix::sys::signal::Signal::SIGVTALRM,
            libc::SIGPROF => nix::sys::signal::Signal::SIGPROF,
            libc::SIGWINCH => nix::sys::signal::Signal::SIGWINCH,
            libc::SIGIO => nix::sys::signal::Signal::SIGIO,
            libc::SIGSYS => nix::sys::signal::Signal::SIGSYS,
            #[cfg(not(any(
                target_os = "android",
                target_os = "emscripten",
                target_os = "fuchsia",
                target_os = "linux",
                target_os = "redox",
                target_os = "haiku"
            )))]
            libc::SIGEMT => nix::sys::signal::Signal::SIGEMT,
            #[cfg(not(any(
                target_os = "android",
                target_os = "emscripten",
                target_os = "fuchsia",
                target_os = "linux",
                target_os = "redox",
                target_os = "haiku",
                target_os = "aix"
            )))]
            libc::SIGINFO => nix::sys::signal::Signal::SIGINFO,
            _ => anyhow::bail!("Unexpected signal number {value}"),
        };
        Ok(rval)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
#[repr(C)]
/// See https://man7.org/linux/man-pages/man2/sigaction.2.html
pub enum SiCodes {
    BUS_ADRALN,
    BUS_ADRERR,
    BUS_MCEERR_AO,
    BUS_MCEERR_AR,
    BUS_OBJERR,
    FPE_FLTDIV,
    FPE_FLTINV,
    FPE_FLTOVF,
    FPE_FLTRES,
    FPE_FLTSUB,
    FPE_FLTUND,
    FPE_INTDIV,
    FPE_INTOVF,
    ILL_BADSTK,
    ILL_COPROC,
    ILL_ILLADR,
    ILL_ILLOPC,
    ILL_ILLOPN,
    ILL_ILLTRP,
    ILL_PRVOPC,
    ILL_PRVREG,
    SEGV_ACCERR,
    SEGV_BNDERR,
    SEGV_MAPERR,
    SEGV_PKUERR,
    SI_ASYNCIO,
    SI_KERNEL,
    SI_MESGQ,
    SI_QUEUE,
    SI_SIGIO,
    SI_TIMER,
    SI_TKILL,
    SI_USER,
    SYS_SECCOMP,
    UNKNOWN,
}

impl SiCodes {
    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "BUS_ADRALN" => Self::BUS_ADRALN,
            "BUS_ADRERR" => Self::BUS_ADRERR,
            "BUS_MCEERR_AO" => Self::BUS_MCEERR_AO,
            "BUS_MCEERR_AR" => Self::BUS_MCEERR_AR,
            "BUS_OBJERR" => Self::BUS_OBJERR,
            "FPE_FLTDIV" => Self::FPE_FLTDIV,
            "FPE_FLTINV" => Self::FPE_FLTINV,
            "FPE_FLTOVF" => Self::FPE_FLTOVF,
            "FPE_FLTRES" => Self::FPE_FLTRES,
            "FPE_FLTSUB" => Self::FPE_FLTSUB,
            "FPE_FLTUND" => Self::FPE_FLTUND,
            "FPE_INTDIV" => Self::FPE_INTDIV,
            "FPE_INTOVF" => Self::FPE_INTOVF,
            "ILL_BADSTK" => Self::ILL_BADSTK,
            "ILL_COPROC" => Self::ILL_COPROC,
            "ILL_ILLADR" => Self::ILL_ILLADR,
            "ILL_ILLOPC" => Self::ILL_ILLOPC,
            "ILL_ILLOPN" => Self::ILL_ILLOPN,
            "ILL_ILLTRP" => Self::ILL_ILLTRP,
            "ILL_PRVOPC" => Self::ILL_PRVOPC,
            "ILL_PRVREG" => Self::ILL_PRVREG,
            "SEGV_ACCERR" => Self::SEGV_ACCERR,
            "SEGV_BNDERR" => Self::SEGV_BNDERR,
            "SEGV_MAPERR" => Self::SEGV_MAPERR,
            "SEGV_PKUERR" => Self::SEGV_PKUERR,
            "SI_ASYNCIO" => Self::SI_ASYNCIO,
            "SI_KERNEL" => Self::SI_KERNEL,
            "SI_MESGQ" => Self::SI_MESGQ,
            "SI_QUEUE" => Self::SI_QUEUE,
            "SI_SIGIO" => Self::SI_SIGIO,
            "SI_TIMER" => Self::SI_TIMER,
            "SI_TKILL" => Self::SI_TKILL,
            "SI_USER" => Self::SI_USER,
            "SYS_SECCOMP" => Self::SYS_SECCOMP,
            _ => Self::UNKNOWN,
        }
    }
}

#[cfg(test)]
impl SigInfo {
    pub fn test_instance(_seed: u64) -> Self {
        Self {
            si_addr: Some("0x0000000000001234".to_string()),
            si_code: 1,
            si_code_human_readable: SiCodes::SEGV_BNDERR,
            si_signo: 11,
            si_signo_human_readable: SignalNames::SIGSEGV,
        }
    }
}
