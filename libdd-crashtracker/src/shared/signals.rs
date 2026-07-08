// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Legacy collector defaults intentionally omit SIGFPE to preserve existing SDK behavior.
#[cfg_attr(not(feature = "std"), allow(dead_code))]
pub const LEGACY_DEFAULT_SIGNALS: [libc::c_int; 4] =
    [libc::SIGBUS, libc::SIGABRT, libc::SIGSEGV, libc::SIGILL];

/// The signal-safe collector uses a fixed crash-signal set that includes SIGFPE.
#[cfg_attr(not(feature = "collector_signal-safe"), allow(dead_code))]
pub const SIGNAL_SAFE_CRASH_SIGNALS: [libc::c_int; 5] = [
    libc::SIGSEGV,
    libc::SIGABRT,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_safe_crash_signals_include_legacy_defaults() {
        for signal in LEGACY_DEFAULT_SIGNALS {
            assert!(SIGNAL_SAFE_CRASH_SIGNALS.contains(&signal));
        }
        assert!(SIGNAL_SAFE_CRASH_SIGNALS.contains(&libc::SIGFPE));
    }
}
