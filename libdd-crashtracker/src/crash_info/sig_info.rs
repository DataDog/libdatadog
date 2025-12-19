// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use num_derive::{FromPrimitive, ToPrimitive};
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
/// See <https://man7.org/linux/man-pages/man7/signal.7.html>
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

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use super::*;

    impl From<libc::c_int> for SignalNames {
        fn from(value: libc::c_int) -> Self {
            match value {
                libc::SIGHUP => SignalNames::SIGHUP,
                libc::SIGINT => SignalNames::SIGINT,
                libc::SIGQUIT => SignalNames::SIGQUIT,
                libc::SIGILL => SignalNames::SIGILL,
                libc::SIGTRAP => SignalNames::SIGTRAP,
                libc::SIGABRT => SignalNames::SIGABRT,
                libc::SIGBUS => SignalNames::SIGBUS,
                libc::SIGFPE => SignalNames::SIGFPE,
                libc::SIGKILL => SignalNames::SIGKILL,
                libc::SIGUSR1 => SignalNames::SIGUSR1,
                libc::SIGSEGV => SignalNames::SIGSEGV,
                libc::SIGUSR2 => SignalNames::SIGUSR2,
                libc::SIGPIPE => SignalNames::SIGPIPE,
                libc::SIGALRM => SignalNames::SIGALRM,
                libc::SIGTERM => SignalNames::SIGTERM,
                libc::SIGCHLD => SignalNames::SIGCHLD,
                libc::SIGCONT => SignalNames::SIGCONT,
                libc::SIGSTOP => SignalNames::SIGSTOP,
                libc::SIGTSTP => SignalNames::SIGTSTP,
                libc::SIGTTIN => SignalNames::SIGTTIN,
                libc::SIGTTOU => SignalNames::SIGTTOU,
                libc::SIGURG => SignalNames::SIGURG,
                libc::SIGXCPU => SignalNames::SIGXCPU,
                libc::SIGXFSZ => SignalNames::SIGXFSZ,
                libc::SIGVTALRM => SignalNames::SIGVTALRM,
                libc::SIGPROF => SignalNames::SIGPROF,
                libc::SIGWINCH => SignalNames::SIGWINCH,
                libc::SIGIO => SignalNames::SIGIO,
                libc::SIGSYS => SignalNames::SIGSYS,
                #[cfg(not(any(
                    target_os = "android",
                    target_os = "emscripten",
                    target_os = "fuchsia",
                    target_os = "linux",
                    target_os = "redox",
                    target_os = "haiku"
                )))]
                libc::SIGEMT => SignalNames::SIGEMT,
                #[cfg(not(any(
                    target_os = "android",
                    target_os = "emscripten",
                    target_os = "fuchsia",
                    target_os = "linux",
                    target_os = "redox",
                    target_os = "haiku",
                    target_os = "aix"
                )))]
                libc::SIGINFO => SignalNames::SIGINFO,
                _ => SignalNames::UNKNOWN,
            }
        }
    }

    extern "C" {
        /// A bit of C code which can access the constants in <signal.h>.
        /// See the file comment on emit_sicodes.c for full details.
        fn translate_si_code_impl(signum: libc::c_int, si_code: libc::c_int) -> libc::c_int;
    }

    pub fn translate_si_code(signum: libc::c_int, si_code: libc::c_int) -> SiCodes {
        use num_traits::FromPrimitive;
        // SAFETY: this function has no safety requirements
        let translated = unsafe { translate_si_code_impl(signum, si_code) };
        SiCodes::from_i32(translated).unwrap_or(SiCodes::UNKNOWN)
    }

    #[cfg(test)]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_si_code() {
        // standard values differ between oses, but it seems like segv match
        // https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/siginfo.h
        // https://github.com/apple/darwin-xnu/blob/main/bsd/sys/signal.h
        assert_eq!(translate_si_code(libc::SIGSEGV, 2), SiCodes::SEGV_ACCERR);

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

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, FromPrimitive, ToPrimitive,
)]
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
#[repr(C)]
/// See <https://man7.org/linux/man-pages/man2/sigaction.2.html>
/// MUST REMAIN IN SYNC WITH THE ENUM IN emit_sigcodes.c
pub enum SiCodes {
    BUS_ADRALN,
    BUS_ADRERR,
    BUS_MCEERR_AO,
    BUS_MCEERR_AR,
    BUS_OBJERR,
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
