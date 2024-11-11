// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use libc;

pub fn get_signal_name(siginfo: &libc::siginfo_t) -> &'static str {
    let signal = siginfo.si_signo;
    match signal {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGBUS => "SIGBUS",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGSYS => "SIGSYS",
        _ => "UNKNOWN",
    }
}

// These are defined in siginfo.h
// They are only derived here because there doesn't appear to be a crate that provides them in a
// comparable way. There's no other reason for this, so it can be replaced as soon as a better
// solution is found.
// (this only copies the most common, actionable values)
pub mod siginfo_code {
    pub const SI_USER: i32 = 0;
    pub const SI_KERNEL: i32 = 0x80;
    pub const SI_QUEUE: i32 = -1;
    pub const SI_TIMER: i32 = -2;
    pub const SI_TKILL: i32 = -6;

    pub mod ill {
        pub const ILLOPC: i32 = 1;
        pub const ILLOPN: i32 = 2;
        pub const ILLADR: i32 = 3;
        pub const ILLTRP: i32 = 4;
        pub const PRVOPC: i32 = 5;
        pub const PRVREG: i32 = 6;
        pub const COPROC: i32 = 7;
        pub const BADSTK: i32 = 8;
    }

    pub mod segv {
        pub const MAPERR: i32 = 1;
        pub const ACCERR: i32 = 2;
    }

    pub mod bus {
        pub const ADRALN: i32 = 1;
        pub const ADRERR: i32 = 2;
        pub const OBJERR: i32 = 3;
    }

    pub mod trap {
        pub const BRKPT: i32 = 1;
        pub const TRACE: i32 = 2;
    }

    pub mod sys {
        pub const SECCOMP: i32 = 1;
    }
}

pub fn get_code_name(siginfo: &libc::siginfo_t) -> &'static str {
    let signo = siginfo.si_signo;

    // Strip out the high byte for PTRACE_EVENT_* flags (defensive coding)
    let code = siginfo.si_code & 0x7f;

    match signo {
        libc::SIGILL => match code {
            siginfo_code::ill::ILLOPC => "ILL_ILLOPC",
            siginfo_code::ill::ILLOPN => "ILL_ILLOPN",
            siginfo_code::ill::ILLADR => "ILL_ILLADR",
            siginfo_code::ill::ILLTRP => "ILL_ILLTRP",
            siginfo_code::ill::PRVOPC => "ILL_PRVOPC",
            siginfo_code::ill::PRVREG => "ILL_PRVREG",
            siginfo_code::ill::COPROC => "ILL_COPROC",
            siginfo_code::ill::BADSTK => "ILL_BADSTK",
            _ => "UNKNOWN_SIGILL",
        },
        libc::SIGSEGV => match code {
            siginfo_code::segv::MAPERR => "SEGV_MAPERR",
            siginfo_code::segv::ACCERR => "SEGV_ACCERR",
            _ => "UNKNOWN_SIGSEGV",
        },
        libc::SIGBUS => match code {
            siginfo_code::bus::ADRALN => "BUS_ADRALN",
            siginfo_code::bus::ADRERR => "BUS_ADRERR",
            siginfo_code::bus::OBJERR => "BUS_OBJERR",
            _ => "UNKNOWN_SIGBUS",
        },
        libc::SIGTRAP => match code {
            siginfo_code::trap::BRKPT => "TRAP_BRKPT",
            siginfo_code::trap::TRACE => "TRAP_TRACE",
            _ => "UNKNOWN_SIGTRAP",
        },
        libc::SIGSYS => match code {
            siginfo_code::sys::SECCOMP => "SYS_SECCOMP",
            _ => "UNKNOWN_SIGSYS",
        },
        _ => match code {
            siginfo_code::SI_USER => "SI_USER",
            siginfo_code::SI_KERNEL => "SI_KERNEL",
            siginfo_code::SI_QUEUE => "SI_QUEUE",
            siginfo_code::SI_TIMER => "SI_TIMER",
            siginfo_code::SI_TKILL => "SI_TKILL",
            _ => "UNKNOWN_GENERAL",
        },
    }
}
