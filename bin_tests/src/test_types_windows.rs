// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows-specific test types for crash tracking tests.
//! Defines crash types (exceptions) and test modes specific to Windows.

/// Represents the different types of crashes (exceptions) that can be triggered on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowsCrashType {
    /// Access violation - null pointer dereference (EXCEPTION_ACCESS_VIOLATION)
    AccessViolationNull,
    /// Access violation - invalid address read
    AccessViolationRead,
    /// Access violation - invalid address write
    AccessViolationWrite,
    /// Integer division by zero (EXCEPTION_INT_DIVIDE_BY_ZERO)
    DivideByZero,
    /// Stack overflow (EXCEPTION_STACK_OVERFLOW)
    StackOverflow,
    /// Illegal instruction (EXCEPTION_ILLEGAL_INSTRUCTION)
    IllegalInstruction,
    /// Explicit abort/panic
    Abort,
}

impl WindowsCrashType {
    /// Returns the string representation used in command-line arguments.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccessViolationNull => "access_violation_null",
            Self::AccessViolationRead => "access_violation_read",
            Self::AccessViolationWrite => "access_violation_write",
            Self::DivideByZero => "divide_by_zero",
            Self::StackOverflow => "stack_overflow",
            Self::IllegalInstruction => "illegal_instruction",
            Self::Abort => "abort",
        }
    }

    /// Returns the expected Windows exception code for this crash type.
    pub const fn exception_code(self) -> u32 {
        match self {
            Self::AccessViolationNull | Self::AccessViolationRead | Self::AccessViolationWrite => {
                0xC0000005
            } // EXCEPTION_ACCESS_VIOLATION
            Self::DivideByZero => 0xC0000094, // EXCEPTION_INT_DIVIDE_BY_ZERO
            Self::StackOverflow => 0xC00000FD, // EXCEPTION_STACK_OVERFLOW
            Self::IllegalInstruction => 0xC000001D, // EXCEPTION_ILLEGAL_INSTRUCTION
            Self::Abort => 0xC0000409,        // STATUS_STACK_BUFFER_OVERRUN (used by abort)
        }
    }

    /// Returns the human-readable exception name.
    pub const fn exception_name(self) -> &'static str {
        match self {
            Self::AccessViolationNull | Self::AccessViolationRead | Self::AccessViolationWrite => {
                "EXCEPTION_ACCESS_VIOLATION"
            }
            Self::DivideByZero => "EXCEPTION_INT_DIVIDE_BY_ZERO",
            Self::StackOverflow => "EXCEPTION_STACK_OVERFLOW",
            Self::IllegalInstruction => "EXCEPTION_ILLEGAL_INSTRUCTION",
            Self::Abort => "STATUS_STACK_BUFFER_OVERRUN",
        }
    }

    /// Returns whether this crash type typically results in incomplete stack traces.
    pub const fn expects_incomplete_stack(self) -> bool {
        matches!(self, Self::StackOverflow)
    }

    /// Returns all available crash types.
    pub const fn all() -> &'static [Self] {
        &[
            Self::AccessViolationNull,
            Self::AccessViolationRead,
            Self::AccessViolationWrite,
            Self::DivideByZero,
            Self::StackOverflow,
            Self::IllegalInstruction,
            Self::Abort,
        ]
    }
}

impl std::fmt::Display for WindowsCrashType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WindowsCrashType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "access_violation_null" => Ok(Self::AccessViolationNull),
            "access_violation_read" => Ok(Self::AccessViolationRead),
            "access_violation_write" => Ok(Self::AccessViolationWrite),
            "divide_by_zero" => Ok(Self::DivideByZero),
            "stack_overflow" => Ok(Self::StackOverflow),
            "illegal_instruction" => Ok(Self::IllegalInstruction),
            "abort" => Ok(Self::Abort),
            _ => Err(format!("Unknown Windows crash type: {}", s)),
        }
    }
}

/// Represents different test modes (behaviors) for Windows crash tracking tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowsTestMode {
    /// Basic crash tracking with no special setup
    Basic,
    /// Multi-threaded crash scenario
    MultiThreaded,
    /// Test with deep call stack
    DeepStack,
    /// Test registry key management
    RegistryTest,
    /// Test with custom WER settings
    CustomWerSettings,
    /// Test WER context validation
    WerContextTest,
}

impl WindowsTestMode {
    /// Returns the string representation used in command-line arguments.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::MultiThreaded => "multithreaded",
            Self::DeepStack => "deepstack",
            Self::RegistryTest => "registry",
            Self::CustomWerSettings => "custom_wer",
            Self::WerContextTest => "wer_context",
        }
    }

    /// Returns all available test modes.
    pub const fn all() -> &'static [Self] {
        &[
            Self::Basic,
            Self::MultiThreaded,
            Self::DeepStack,
            Self::RegistryTest,
            Self::CustomWerSettings,
            Self::WerContextTest,
        ]
    }
}

impl std::fmt::Display for WindowsTestMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WindowsTestMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "basic" => Ok(Self::Basic),
            "multithreaded" => Ok(Self::MultiThreaded),
            "deepstack" => Ok(Self::DeepStack),
            "registry" => Ok(Self::RegistryTest),
            "custom_wer" => Ok(Self::CustomWerSettings),
            "wer_context" => Ok(Self::WerContextTest),
            _ => Err(format!("Unknown Windows test mode: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crash_type_str_roundtrip() {
        for crash_type in WindowsCrashType::all() {
            let s = crash_type.as_str();
            let parsed: WindowsCrashType = s.parse().unwrap();
            assert_eq!(*crash_type, parsed);
        }
    }

    #[test]
    fn test_exception_codes() {
        assert_eq!(
            WindowsCrashType::AccessViolationNull.exception_code(),
            0xC0000005
        );
        assert_eq!(WindowsCrashType::DivideByZero.exception_code(), 0xC0000094);
        assert_eq!(WindowsCrashType::StackOverflow.exception_code(), 0xC00000FD);
    }

    #[test]
    fn test_mode_str_roundtrip() {
        for mode in WindowsTestMode::all() {
            let s = mode.as_str();
            let parsed: WindowsTestMode = s.parse().unwrap();
            assert_eq!(*mode, parsed);
        }
    }

    #[test]
    fn test_incomplete_stack_expectation() {
        assert!(WindowsCrashType::StackOverflow.expects_incomplete_stack());
        assert!(!WindowsCrashType::AccessViolationNull.expects_incomplete_stack());
    }
}
