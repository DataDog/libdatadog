// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Represents the different test modes (behaviors) available for crash tracking tests.
/// Each mode corresponds to a specific test scenario (e.g., signal handling, fork, chaining).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestMode {
    DoNothing,
    SigPipe,
    SigChld,
    SigChldExec,
    DoNothingSigStack,
    SigPipeSigStack,
    SigChldSigStack,
    Chained,
    Fork,
    PrechainAbort,
    RuntimeCallbackFrame,
    RuntimeCallbackString,
    RuntimeCallbackFrameInvalidUtf8,
}

impl TestMode {
    /// Returns the string representation used in command-line arguments and behavior mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DoNothing => "donothing",
            Self::SigPipe => "sigpipe",
            Self::SigChld => "sigchld",
            Self::SigChldExec => "sigchld_exec",
            Self::DoNothingSigStack => "donothing_sigstack",
            Self::SigPipeSigStack => "sigpipe_sigstack",
            Self::SigChldSigStack => "sigchld_sigstack",
            Self::Chained => "chained",
            Self::Fork => "fork",
            Self::PrechainAbort => "prechain_abort",
            Self::RuntimeCallbackFrame => "runtime_callback_frame",
            Self::RuntimeCallbackString => "runtime_callback_string",
            Self::RuntimeCallbackFrameInvalidUtf8 => "runtime_callback_frame_invalid_utf8",
        }
    }

    /// Returns all available test modes.
    pub const fn all() -> &'static [Self] {
        &[
            Self::DoNothing,
            Self::SigPipe,
            Self::SigChld,
            Self::SigChldExec,
            Self::DoNothingSigStack,
            Self::SigPipeSigStack,
            Self::SigChldSigStack,
            Self::Chained,
            Self::Fork,
            Self::PrechainAbort,
            Self::RuntimeCallbackFrame,
            Self::RuntimeCallbackString,
            Self::RuntimeCallbackFrameInvalidUtf8,
        ]
    }
}

impl std::fmt::Display for TestMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TestMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "donothing" => Ok(Self::DoNothing),
            "sigpipe" => Ok(Self::SigPipe),
            "sigchld" => Ok(Self::SigChld),
            "sigchld_exec" => Ok(Self::SigChldExec),
            "donothing_sigstack" => Ok(Self::DoNothingSigStack),
            "sigpipe_sigstack" => Ok(Self::SigPipeSigStack),
            "sigchld_sigstack" => Ok(Self::SigChldSigStack),
            "chained" => Ok(Self::Chained),
            "fork" => Ok(Self::Fork),
            "prechain_abort" => Ok(Self::PrechainAbort),
            "runtime_callback_frame" => Ok(Self::RuntimeCallbackFrame),
            "runtime_callback_string" => Ok(Self::RuntimeCallbackString),
            "runtime_callback_frame_invalid_utf8" => Ok(Self::RuntimeCallbackFrameInvalidUtf8),
            _ => Err(format!("Unknown test mode: {}", s)),
        }
    }
}

/// Represents the different types of crashes that can be triggered in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrashType {
    /// Null pointer dereference (SIGSEGV via inline assembly)
    NullDeref,
    /// Kill process with SIGABRT
    KillSigAbrt,
    /// Kill process with SIGILL
    KillSigIll,
    /// Kill process with SIGBUS
    KillSigBus,
    /// Kill process with SIGSEGV
    KillSigSegv,
    /// Raise SIGABRT
    RaiseSigAbrt,
    /// Raise SIGILL
    RaiseSigIll,
    /// Raise SIGBUS
    RaiseSigBus,
    /// Raise SIGSEGV
    RaiseSigSegv,
}

impl CrashType {
    /// Returns the string representation used in command-line arguments.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NullDeref => "null_deref",
            Self::KillSigAbrt => "kill_sigabrt",
            Self::KillSigIll => "kill_sigill",
            Self::KillSigBus => "kill_sigbus",
            Self::KillSigSegv => "kill_sigsegv",
            Self::RaiseSigAbrt => "raise_sigabrt",
            Self::RaiseSigIll => "raise_sigill",
            Self::RaiseSigBus => "raise_sigbus",
            Self::RaiseSigSegv => "raise_sigsegv",
        }
    }

    /// Returns whether this crash type should result in a successful exit code.
    /// Some signal types (SIGBUS, SIGSEGV via kill) may be caught and handled,
    /// resulting in a clean exit.
    pub const fn expects_success(self) -> bool {
        matches!(
            self,
            Self::KillSigBus | Self::KillSigSegv | Self::RaiseSigBus | Self::RaiseSigSegv
        )
    }

    /// Returns the expected signal number for this crash type (Unix only).
    #[cfg(unix)]
    pub const fn signal_number(self) -> i32 {
        match self {
            Self::NullDeref | Self::KillSigSegv | Self::RaiseSigSegv => 11, // SIGSEGV
            Self::KillSigAbrt | Self::RaiseSigAbrt => 6,                    // SIGABRT
            Self::KillSigIll | Self::RaiseSigIll => 4,                      // SIGILL
            Self::KillSigBus | Self::RaiseSigBus => 7,                      // SIGBUS
        }
    }

    /// Returns the human-readable signal name for this crash type.
    pub const fn signal_name(self) -> &'static str {
        match self {
            Self::NullDeref | Self::KillSigSegv | Self::RaiseSigSegv => "SIGSEGV",
            Self::KillSigAbrt | Self::RaiseSigAbrt => "SIGABRT",
            Self::KillSigIll | Self::RaiseSigIll => "SIGILL",
            Self::KillSigBus | Self::RaiseSigBus => "SIGBUS",
        }
    }
}

impl std::fmt::Display for CrashType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CrashType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "null_deref" => Ok(Self::NullDeref),
            "kill_sigabrt" => Ok(Self::KillSigAbrt),
            "kill_sigill" => Ok(Self::KillSigIll),
            "kill_sigbus" => Ok(Self::KillSigBus),
            "kill_sigsegv" => Ok(Self::KillSigSegv),
            "raise_sigabrt" => Ok(Self::RaiseSigAbrt),
            "raise_sigill" => Ok(Self::RaiseSigIll),
            "raise_sigbus" => Ok(Self::RaiseSigBus),
            "raise_sigsegv" => Ok(Self::RaiseSigSegv),
            _ => Err(format!("Unknown crash type: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_str_roundtrip() {
        for mode in TestMode::all() {
            let s = mode.as_str();
            let parsed: TestMode = s.parse().unwrap();
            assert_eq!(*mode, parsed);
        }
    }

    #[test]
    fn test_crash_type_signal_info() {
        assert_eq!(CrashType::NullDeref.signal_name(), "SIGSEGV");
        assert_eq!(CrashType::KillSigAbrt.signal_name(), "SIGABRT");

        #[cfg(unix)]
        {
            assert_eq!(CrashType::NullDeref.signal_number(), 11);
            assert_eq!(CrashType::KillSigAbrt.signal_number(), 6);
        }
    }

    #[test]
    fn test_crash_type_expects_success() {
        assert!(!CrashType::NullDeref.expects_success());
        assert!(!CrashType::KillSigAbrt.expects_success());
        assert!(CrashType::KillSigBus.expects_success());
        assert!(CrashType::KillSigSegv.expects_success());
    }
}
