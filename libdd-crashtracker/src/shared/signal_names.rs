// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg_attr(not(feature = "collector_signal-safe"), allow(dead_code))]
pub fn rust_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGURG => "SIGURG",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGVTALRM => "SIGVTALRM",
        libc::SIGPROF => "SIGPROF",
        libc::SIGWINCH => "SIGWINCH",
        libc::SIGIO => "SIGIO",
        libc::SIGSYS => "SIGSYS",
        #[cfg(not(any(
            target_os = "android",
            target_os = "emscripten",
            target_os = "fuchsia",
            target_os = "linux",
            target_os = "redox",
            target_os = "haiku"
        )))]
        libc::SIGEMT => "SIGEMT",
        #[cfg(not(any(
            target_os = "android",
            target_os = "emscripten",
            target_os = "fuchsia",
            target_os = "linux",
            target_os = "redox",
            target_os = "haiku",
            target_os = "aix"
        )))]
        libc::SIGINFO => "SIGINFO",
        _ => "UNKNOWN",
    }
}

pub fn rust_si_code_name(signal: i32, si_code: i32) -> &'static str {
    match si_code {
        SI_USER => "SI_USER",
        SI_KERNEL => "SI_KERNEL",
        SI_QUEUE => "SI_QUEUE",
        SI_TIMER => "SI_TIMER",
        SI_MESGQ => "SI_MESGQ",
        SI_ASYNCIO => "SI_ASYNCIO",
        SI_SIGIO => "SI_SIGIO",
        SI_TKILL => "SI_TKILL",
        _ => signal_specific_si_code_name(signal, si_code),
    }
}

#[cfg_attr(not(feature = "collector_signal-safe"), allow(dead_code))]
pub fn signal_has_address(signal: i32) -> bool {
    matches!(
        signal,
        libc::SIGBUS | libc::SIGFPE | libc::SIGILL | libc::SIGSEGV | libc::SIGTRAP
    )
}

fn signal_specific_si_code_name(signal: i32, si_code: i32) -> &'static str {
    match signal {
        libc::SIGSEGV => match si_code {
            SEGV_MAPERR => "SEGV_MAPERR",
            SEGV_ACCERR => "SEGV_ACCERR",
            SEGV_BNDERR => "SEGV_BNDERR",
            SEGV_PKUERR => "SEGV_PKUERR",
            _ => "UNKNOWN",
        },
        libc::SIGBUS => match si_code {
            BUS_ADRALN => "BUS_ADRALN",
            BUS_ADRERR => "BUS_ADRERR",
            BUS_OBJERR => "BUS_OBJERR",
            BUS_MCEERR_AR => "BUS_MCEERR_AR",
            BUS_MCEERR_AO => "BUS_MCEERR_AO",
            _ => "UNKNOWN",
        },
        libc::SIGILL => match si_code {
            ILL_ILLOPC => "ILL_ILLOPC",
            ILL_ILLOPN => "ILL_ILLOPN",
            ILL_ILLADR => "ILL_ILLADR",
            ILL_ILLTRP => "ILL_ILLTRP",
            ILL_PRVOPC => "ILL_PRVOPC",
            ILL_PRVREG => "ILL_PRVREG",
            ILL_COPROC => "ILL_COPROC",
            ILL_BADSTK => "ILL_BADSTK",
            _ => "UNKNOWN",
        },
        libc::SIGFPE => match si_code {
            FPE_INTDIV => "FPE_INTDIV",
            FPE_INTOVF => "FPE_INTOVF",
            FPE_FLTDIV => "FPE_FLTDIV",
            FPE_FLTOVF => "FPE_FLTOVF",
            FPE_FLTUND => "FPE_FLTUND",
            FPE_FLTRES => "FPE_FLTRES",
            FPE_FLTINV => "FPE_FLTINV",
            FPE_FLTSUB => "FPE_FLTSUB",
            _ => "UNKNOWN",
        },
        _ => "UNKNOWN",
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_USER: i32 = libc::SI_USER;
#[cfg(target_vendor = "apple")]
pub const SI_USER: i32 = 0x10001;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_USER: i32 = 0;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_KERNEL: i32 = libc::SI_KERNEL;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SI_KERNEL: i32 = i32::MIN;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_QUEUE: i32 = libc::SI_QUEUE;
#[cfg(target_vendor = "apple")]
pub const SI_QUEUE: i32 = 0x10002;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_QUEUE: i32 = -1;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_TIMER: i32 = libc::SI_TIMER;
#[cfg(target_vendor = "apple")]
pub const SI_TIMER: i32 = 0x10003;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_TIMER: i32 = -2;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_MESGQ: i32 = libc::SI_MESGQ;
#[cfg(target_vendor = "apple")]
pub const SI_MESGQ: i32 = 0x10005;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_MESGQ: i32 = -3;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_ASYNCIO: i32 = libc::SI_ASYNCIO;
#[cfg(target_vendor = "apple")]
pub const SI_ASYNCIO: i32 = 0x10004;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_ASYNCIO: i32 = -4;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_SIGIO: i32 = libc::SI_SIGIO;
#[cfg(target_vendor = "apple")]
pub const SI_SIGIO: i32 = i32::MIN + 5;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub const SI_SIGIO: i32 = -5;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_TKILL: i32 = libc::SI_TKILL;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SI_TKILL: i32 = i32::MIN + 6;

pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SEGV_BNDERR: i32 = 3;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SEGV_BNDERR: i32 = i32::MIN + 3;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SEGV_PKUERR: i32 = 4;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SEGV_PKUERR: i32 = i32::MIN + 4;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const BUS_ADRALN: i32 = libc::BUS_ADRALN;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const BUS_ADRALN: i32 = 1;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const BUS_ADRERR: i32 = libc::BUS_ADRERR;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const BUS_ADRERR: i32 = 2;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const BUS_OBJERR: i32 = libc::BUS_OBJERR;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const BUS_OBJERR: i32 = 3;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const BUS_MCEERR_AR: i32 = libc::BUS_MCEERR_AR;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const BUS_MCEERR_AR: i32 = i32::MIN + 1;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const BUS_MCEERR_AO: i32 = libc::BUS_MCEERR_AO;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const BUS_MCEERR_AO: i32 = i32::MIN + 2;

pub const ILL_ILLOPC: i32 = 1;

#[cfg(target_vendor = "apple")]
pub const ILL_ILLTRP: i32 = 2;
#[cfg(not(target_vendor = "apple"))]
pub const ILL_ILLOPN: i32 = 2;

#[cfg(target_vendor = "apple")]
pub const ILL_PRVOPC: i32 = 3;
#[cfg(not(target_vendor = "apple"))]
pub const ILL_ILLADR: i32 = 3;

#[cfg(target_vendor = "apple")]
pub const ILL_ILLOPN: i32 = 4;
#[cfg(not(target_vendor = "apple"))]
pub const ILL_ILLTRP: i32 = 4;

#[cfg(target_vendor = "apple")]
pub const ILL_ILLADR: i32 = 5;
#[cfg(not(target_vendor = "apple"))]
pub const ILL_PRVOPC: i32 = 5;

pub const ILL_PRVREG: i32 = 6;
pub const ILL_COPROC: i32 = 7;
pub const ILL_BADSTK: i32 = 8;

#[cfg(not(target_vendor = "apple"))]
pub const FPE_INTDIV: i32 = 1;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_INTOVF: i32 = 2;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTDIV: i32 = 3;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTOVF: i32 = 4;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTUND: i32 = 5;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTRES: i32 = 6;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTINV: i32 = 7;
#[cfg(not(target_vendor = "apple"))]
pub const FPE_FLTSUB: i32 = 8;

#[cfg(target_vendor = "apple")]
pub const FPE_FLTDIV: i32 = 1;
#[cfg(target_vendor = "apple")]
pub const FPE_FLTOVF: i32 = 2;
#[cfg(target_vendor = "apple")]
pub const FPE_FLTUND: i32 = 3;
#[cfg(target_vendor = "apple")]
pub const FPE_FLTRES: i32 = 4;
#[cfg(target_vendor = "apple")]
pub const FPE_FLTINV: i32 = 5;
#[cfg(target_vendor = "apple")]
pub const FPE_FLTSUB: i32 = 6;
#[cfg(target_vendor = "apple")]
pub const FPE_INTDIV: i32 = 7;
#[cfg(target_vendor = "apple")]
pub const FPE_INTOVF: i32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_names_cover_common_native_faults() {
        assert_eq!(rust_signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(rust_signal_name(libc::SIGABRT), "SIGABRT");
        assert_eq!(rust_signal_name(libc::SIGBUS), "SIGBUS");
        assert_eq!(rust_signal_name(libc::SIGILL), "SIGILL");
        assert_eq!(rust_signal_name(libc::SIGFPE), "SIGFPE");
        assert_eq!(rust_signal_name(999), "UNKNOWN");
    }

    #[test]
    fn si_code_names_cover_common_native_faults() {
        assert_eq!(rust_si_code_name(libc::SIGSEGV, SEGV_MAPERR), "SEGV_MAPERR");
        assert_eq!(rust_si_code_name(libc::SIGBUS, BUS_ADRALN), "BUS_ADRALN");
        assert_eq!(rust_si_code_name(libc::SIGILL, ILL_ILLOPC), "ILL_ILLOPC");
        assert_eq!(rust_si_code_name(libc::SIGFPE, FPE_INTDIV), "FPE_INTDIV");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, SI_USER), "SI_USER");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, 999), "UNKNOWN");
    }
}
