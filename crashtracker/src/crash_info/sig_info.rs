// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
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
    SIGABRT,
    SIGBUS,
    SIGSEGV,
    SIGSYS,
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
                libc::SIGABRT => SignalNames::SIGABRT,
                libc::SIGBUS => SignalNames::SIGBUS,
                libc::SIGSEGV => SignalNames::SIGSEGV,
                libc::SIGSYS => SignalNames::SIGSYS,
                _ => panic!("Unexpected signal number: {value}"),
            }
        }
    }

    extern "C" {
        fn translate_si_code_impl(signum: libc::c_int, si_code: libc::c_int) -> libc::c_int;
    }

    pub fn translate_si_code(signum: libc::c_int, si_code: libc::c_int) -> SiCodes {
        // SAFETY: this function has no safety requirements
        let translated = unsafe { translate_si_code_impl(signum, si_code) };
        SiCodes::from_i32(translated).unwrap_or(SiCodes::UNKNOWN)
    }

    #[cfg(test)]
    #[test]
    fn test_si_code() {
        // standard values differ between oses, but it seems like segv match
        // https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/siginfo.h for some
        //  https://github.com/apple/darwin-xnu/blob/main/bsd/sys/signal.h
        assert_eq!(translate_si_code(libc::SIGSEGV, 42), SiCodes::UNKNOWN);
        assert_eq!(translate_si_code(libc::SIGSEGV, 2), SiCodes::SEGV_ACCERR);
    }
}

#[cfg(not(unix))]
impl From<libc::c_int> for SignalNames {
    fn from(_value: libc::c_int) -> Self {
        unreachable!("Non-unix systems should not have Signals")
    }
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, FromPrimitive, ToPrimitive,
)]
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
#[repr(C)]
/// See https://man7.org/linux/man-pages/man2/sigaction.2.html
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
