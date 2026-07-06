// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub fn rust_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
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
            _ => "UNKNOWN",
        },
        libc::SIGBUS => match si_code {
            BUS_ADRALN => "BUS_ADRALN",
            BUS_ADRERR => "BUS_ADRERR",
            BUS_OBJERR => "BUS_OBJERR",
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
        // FPE_* values are deliberately reported as UNKNOWN until the receiver model has
        // corresponding enum variants.
        _ => "UNKNOWN",
    }
}

pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 128;
pub const SI_QUEUE: i32 = -1;
pub const SI_TIMER: i32 = -2;
pub const SI_MESGQ: i32 = -3;
pub const SI_ASYNCIO: i32 = -4;
pub const SI_SIGIO: i32 = -5;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_TKILL: i32 = -6;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
// Non-Linux platforms do not define SI_TKILL; use a sentinel that cannot match a real si_code.
pub const SI_TKILL: i32 = i32::MIN;

pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;

pub const BUS_ADRALN: i32 = 1;
pub const BUS_ADRERR: i32 = 2;
pub const BUS_OBJERR: i32 = 3;

pub const ILL_ILLOPC: i32 = 1;
pub const ILL_ILLOPN: i32 = 2;
pub const ILL_ILLADR: i32 = 3;
pub const ILL_ILLTRP: i32 = 4;
pub const ILL_PRVOPC: i32 = 5;
pub const ILL_PRVREG: i32 = 6;
pub const ILL_COPROC: i32 = 7;
pub const ILL_BADSTK: i32 = 8;

pub const FPE_INTDIV: i32 = 1;
pub const FPE_INTOVF: i32 = 2;
pub const FPE_FLTDIV: i32 = 3;
pub const FPE_FLTOVF: i32 = 4;
pub const FPE_FLTUND: i32 = 5;
pub const FPE_FLTRES: i32 = 6;
pub const FPE_FLTINV: i32 = 7;
pub const FPE_FLTSUB: i32 = 8;

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
        assert_eq!(rust_si_code_name(libc::SIGSEGV, SI_USER), "SI_USER");
        assert_eq!(rust_si_code_name(libc::SIGFPE, FPE_INTDIV), "UNKNOWN");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, 999), "UNKNOWN");
    }
}
