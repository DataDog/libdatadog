// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};

use crate::rules_based::{ExpectedFlagType, FlagType, Str};

/// Enum representing all possible reasons that could result in evaluation returning an error or
/// default assignment.
///
/// Not all of these are technically "errors"—some can be expected to occur frequently (e.g.,
/// `FlagDisabled` or `DefaultAllocation`).
///
/// Dependency sources are shared so trace records retain linear state. Serialization
/// represents a dependency chain as `prerequisites` (outermost first) and a terminal
/// `cause`, rather than recursively nesting errors. Nondependency wire forms are unchanged.
/// Destruction unlinks dependency errors iteratively. Because this type implements `Drop`,
/// inspect owned fields by reference rather than moving them out of an error.
#[derive(thiserror::Error, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "ErrorWire", into = "ErrorWire")]
#[non_exhaustive]
pub enum EvaluationError {
    /// A prerequisite failed. Its cause is preserved, while SDKs can distinguish this from
    /// a directly requested flag's normal disabled/default result.
    #[error("prerequisite {flag_key} failed: {}", DependencyErrorDisplay(.source))]
    DependencyEvaluationFailed {
        /// Referenced prerequisite key.
        flag_key: Str,
        /// Original prerequisite failure.
        source: EvaluationErrorSource,
    },
    /// A reached prerequisite references a flag already being evaluated.
    #[error("flag dependency cycle")]
    DependencyCycle,

    /// A reached prerequisite exceeds the configuration's dependency depth limit.
    #[error("flag dependency depth exceeded")]
    DependencyDepthExceeded,
    /// Requested flag has unexpected type.
    #[error("invalid flag type (expected: {expected:?}, found: {found:?})")]
    TypeMismatch {
        /// Expected type of the flag.
        expected: ExpectedFlagType,
        /// Actual type of the flag.
        found: FlagType,
    },

    /// Targeting key is required for evaluation but is missing.
    #[error("targeting key is missing")]
    TargetingKeyMissing,

    /// The enabled experimentation opt-out parameter is not an RFC3339 string.
    #[error(
        "dd_experimentation_opt_out_at must be an RFC3339 timestamp string; supply a valid timestamp to opt out, or omit the attribute to permit experimentation"
    )]
    InvalidContext,

    /// Failed to parse configuration. This should normally never happen and is likely a signal
    /// that you should update SDK.
    #[error("failed to parse configuration")]
    ConfigurationParseError,

    /// A requested flag exists in the configuration payload, but its per-flag configuration is
    /// invalid or unsupported by this SDK. Failures encountered while compiling an individual
    /// flag are converted to `FlagConfigurationInvalid` at the per-flag ingestion boundary. The
    /// SDK should return the caller default and expose this as a parse error for that flag without
    /// invalidating the rest of the configuration.
    #[error("flag configuration is invalid or unsupported")]
    FlagConfigurationInvalid,

    /// Configuration has not been fetched yet.
    #[error("flags configuration is missing")]
    ConfigurationMissing,

    /// The requested flag configuration was not found. It either does not exist or is disabled.
    #[error("flag is missing in configuration, it is either unrecognized or disabled")]
    FlagUnrecognizedOrDisabled,

    /// Flag is found in configuration but it is disabled.
    #[error("flag is disabled")]
    FlagDisabled,

    /// Default allocation is matched and is also serving `NULL`, resulting in the default value
    /// being assigned.
    #[error("default allocation is matched and is serving NULL")]
    DefaultAllocationNull,

    #[error("internal error: {0}")]
    Internal(Str),
}

/// A shared prerequisite failure with iterative equality.
///
/// Dereferences to the original error. Keeping equality on this wrapper allows
/// `EvaluationError` to retain derived structural equality for constant patterns.
#[derive(Clone, Debug)]
pub struct EvaluationErrorSource(Arc<EvaluationError>);

impl From<EvaluationError> for EvaluationErrorSource {
    fn from(error: EvaluationError) -> Self {
        Self(Arc::new(error))
    }
}

impl std::ops::Deref for EvaluationErrorSource {
    type Target = EvaluationError;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<EvaluationError> for EvaluationErrorSource {
    fn as_ref(&self) -> &EvaluationError {
        &self.0
    }
}

impl fmt::Display for EvaluationErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_ref(), formatter)
    }
}

impl std::error::Error for EvaluationErrorSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(self.as_ref())
    }
}

impl PartialEq for EvaluationErrorSource {
    fn eq(&self, other: &Self) -> bool {
        let mut left = self.as_ref();
        let mut right = other.as_ref();
        loop {
            match (left, right) {
                (
                    EvaluationError::DependencyEvaluationFailed {
                        flag_key: a,
                        source: next_a,
                    },
                    EvaluationError::DependencyEvaluationFailed {
                        flag_key: b,
                        source: next_b,
                    },
                ) => {
                    if a != b {
                        return false;
                    }
                    left = next_a;
                    right = next_b;
                }
                // At least one side is terminal: derived equality cannot recurse.
                _ => return left == right,
            }
        }
    }
}

// Shared suffixes stop cleanup; uniquely owned suffixes are unlinked iteratively.
impl Drop for EvaluationError {
    fn drop(&mut self) {
        fn unlink(error: &mut EvaluationError) -> Option<EvaluationError> {
            if let EvaluationError::DependencyEvaluationFailed { source, .. } = error {
                Arc::get_mut(&mut source.0)
                    .map(|inner| std::mem::replace(inner, EvaluationError::DependencyCycle))
            } else {
                None
            }
        }
        if let Some(mut next) = unlink(self) {
            while let Some(inner) = unlink(&mut next) {
                next = inner;
            }
        }
    }
}

// Error logging must not reintroduce recursion after dependency evaluation has
// moved to an explicit stack. Preserve the same message and source-chain API.
struct DependencyErrorDisplay<'a>(&'a EvaluationError);

impl fmt::Display for DependencyErrorDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut error = self.0;
        while let EvaluationError::DependencyEvaluationFailed { flag_key, source } = error {
            write!(formatter, "prerequisite {flag_key} failed: ")?;
            error = source;
        }
        fmt::Display::fmt(error, formatter)
    }
}

// A flat wire representation keeps serializers and deserializers off the dependency stack.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ErrorCause {
    DependencyCycle,
    DependencyDepthExceeded,
    TargetingKeyMissing,
    InvalidContext,
    ConfigurationParseError,
    FlagConfigurationInvalid,
    ConfigurationMissing,
    FlagUnrecognizedOrDisabled,
    FlagDisabled,
    DefaultAllocationNull,
    TypeMismatch {
        expected: ExpectedFlagType,
        found: FlagType,
    },
    Internal(Str),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ErrorWire {
    // Preserve the original variant order for nondependency bincode payloads.
    TypeMismatch {
        expected: ExpectedFlagType,
        found: FlagType,
    },
    TargetingKeyMissing,
    InvalidContext,
    ConfigurationParseError,
    FlagConfigurationInvalid,
    ConfigurationMissing,
    FlagUnrecognizedOrDisabled,
    FlagDisabled,
    DefaultAllocationNull,
    Internal(Str),
    DependencyEvaluationFailed {
        prerequisites: Vec<Str>,
        cause: ErrorCause,
    },
    DependencyCycle,
    DependencyDepthExceeded,
}

impl EvaluationError {
    fn chain(&self) -> (Vec<Str>, ErrorCause) {
        let mut prerequisites = Vec::new();
        let mut error = self;
        while let Self::DependencyEvaluationFailed { flag_key, source } = error {
            prerequisites.push(flag_key.clone());
            error = source;
        }
        let cause = match error {
            Self::DependencyCycle => ErrorCause::DependencyCycle,
            Self::DependencyDepthExceeded => ErrorCause::DependencyDepthExceeded,
            Self::TargetingKeyMissing => ErrorCause::TargetingKeyMissing,
            Self::InvalidContext => ErrorCause::InvalidContext,
            Self::ConfigurationParseError => ErrorCause::ConfigurationParseError,
            Self::FlagConfigurationInvalid => ErrorCause::FlagConfigurationInvalid,
            Self::ConfigurationMissing => ErrorCause::ConfigurationMissing,
            Self::FlagUnrecognizedOrDisabled => ErrorCause::FlagUnrecognizedOrDisabled,
            Self::FlagDisabled => ErrorCause::FlagDisabled,
            Self::DefaultAllocationNull => ErrorCause::DefaultAllocationNull,
            Self::TypeMismatch { expected, found } => ErrorCause::TypeMismatch {
                expected: *expected,
                found: *found,
            },
            Self::Internal(message) => ErrorCause::Internal(message.clone()),
            Self::DependencyEvaluationFailed { .. } => unreachable!(),
        };
        (prerequisites, cause)
    }
}

impl From<EvaluationError> for ErrorWire {
    fn from(error: EvaluationError) -> Self {
        let (prerequisites, cause) = error.chain();
        if !prerequisites.is_empty() {
            return Self::DependencyEvaluationFailed {
                prerequisites,
                cause,
            };
        }
        match cause {
            ErrorCause::DependencyCycle => Self::DependencyCycle,
            ErrorCause::DependencyDepthExceeded => Self::DependencyDepthExceeded,
            ErrorCause::TargetingKeyMissing => Self::TargetingKeyMissing,
            ErrorCause::InvalidContext => Self::InvalidContext,
            ErrorCause::ConfigurationParseError => Self::ConfigurationParseError,
            ErrorCause::FlagConfigurationInvalid => Self::FlagConfigurationInvalid,
            ErrorCause::ConfigurationMissing => Self::ConfigurationMissing,
            ErrorCause::FlagUnrecognizedOrDisabled => Self::FlagUnrecognizedOrDisabled,
            ErrorCause::FlagDisabled => Self::FlagDisabled,
            ErrorCause::DefaultAllocationNull => Self::DefaultAllocationNull,
            ErrorCause::TypeMismatch { expected, found } => Self::TypeMismatch { expected, found },
            ErrorCause::Internal(message) => Self::Internal(message),
        }
    }
}

impl From<ErrorCause> for EvaluationError {
    fn from(cause: ErrorCause) -> Self {
        match cause {
            ErrorCause::DependencyCycle => Self::DependencyCycle,
            ErrorCause::DependencyDepthExceeded => Self::DependencyDepthExceeded,
            ErrorCause::TargetingKeyMissing => Self::TargetingKeyMissing,
            ErrorCause::InvalidContext => Self::InvalidContext,
            ErrorCause::ConfigurationParseError => Self::ConfigurationParseError,
            ErrorCause::FlagConfigurationInvalid => Self::FlagConfigurationInvalid,
            ErrorCause::ConfigurationMissing => Self::ConfigurationMissing,
            ErrorCause::FlagUnrecognizedOrDisabled => Self::FlagUnrecognizedOrDisabled,
            ErrorCause::FlagDisabled => Self::FlagDisabled,
            ErrorCause::DefaultAllocationNull => Self::DefaultAllocationNull,
            ErrorCause::TypeMismatch { expected, found } => Self::TypeMismatch { expected, found },
            ErrorCause::Internal(message) => Self::Internal(message),
        }
    }
}

impl From<ErrorWire> for EvaluationError {
    fn from(wire: ErrorWire) -> Self {
        match wire {
            ErrorWire::DependencyEvaluationFailed {
                prerequisites,
                cause,
            } => {
                let mut error = Self::from(cause);
                for flag_key in prerequisites.into_iter().rev() {
                    error = Self::DependencyEvaluationFailed {
                        flag_key,
                        source: error.into(),
                    };
                }
                error
            }
            ErrorWire::DependencyCycle => Self::DependencyCycle,
            ErrorWire::DependencyDepthExceeded => Self::DependencyDepthExceeded,
            ErrorWire::TargetingKeyMissing => Self::TargetingKeyMissing,
            ErrorWire::InvalidContext => Self::InvalidContext,
            ErrorWire::ConfigurationParseError => Self::ConfigurationParseError,
            ErrorWire::FlagConfigurationInvalid => Self::FlagConfigurationInvalid,
            ErrorWire::ConfigurationMissing => Self::ConfigurationMissing,
            ErrorWire::FlagUnrecognizedOrDisabled => Self::FlagUnrecognizedOrDisabled,
            ErrorWire::FlagDisabled => Self::FlagDisabled,
            ErrorWire::DefaultAllocationNull => Self::DefaultAllocationNull,
            ErrorWire::TypeMismatch { expected, found } => Self::TypeMismatch { expected, found },
            ErrorWire::Internal(message) => Self::Internal(message),
        }
    }
}

impl fmt::Debug for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prerequisites, cause) = self.chain();
        formatter
            .debug_struct("EvaluationError")
            .field("prerequisites", &prerequisites)
            .field("cause", &cause)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_patterns_and_shared_suffixes_remain_supported() {
        const DISABLED: EvaluationError = EvaluationError::FlagDisabled;
        assert!(matches!(EvaluationError::FlagDisabled, DISABLED));
        let source = EvaluationErrorSource::from(EvaluationError::FlagDisabled);
        let cloned = source.clone();
        assert!(Arc::ptr_eq(&source.0, &cloned.0));
        assert_ne!(
            source,
            EvaluationErrorSource::from(EvaluationError::DependencyCycle)
        );
    }

    #[test]
    fn error_wire_round_trips_and_preserves_terminal_forms() {
        for error in [
            EvaluationError::DependencyCycle,
            EvaluationError::DependencyDepthExceeded,
            EvaluationError::TargetingKeyMissing,
            EvaluationError::InvalidContext,
            EvaluationError::ConfigurationParseError,
            EvaluationError::FlagConfigurationInvalid,
            EvaluationError::ConfigurationMissing,
            EvaluationError::FlagUnrecognizedOrDisabled,
            EvaluationError::FlagDisabled,
            EvaluationError::DefaultAllocationNull,
            EvaluationError::TypeMismatch {
                expected: ExpectedFlagType::Boolean,
                found: FlagType::String,
            },
            EvaluationError::Internal("owned message".to_owned().into()),
        ] {
            let terminal_json = serde_json::to_value(&error).unwrap();
            let decoded: EvaluationError = serde_json::from_value(terminal_json.clone()).unwrap();
            assert_eq!(decoded, error);
            let nested = EvaluationError::DependencyEvaluationFailed {
                flag_key: "parent".into(),
                source: error.into(),
            };
            let encoded = serde_json::to_value(&nested).unwrap();
            assert_eq!(
                encoded["DEPENDENCY_EVALUATION_FAILED"]["prerequisites"],
                serde_json::json!(["parent"])
            );
            assert_eq!(
                encoded["DEPENDENCY_EVALUATION_FAILED"]["cause"],
                terminal_json
            );
            let decoded: EvaluationError = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, nested);
            let decoded: EvaluationError =
                bincode::deserialize(&bincode::serialize(&nested).unwrap()).unwrap();
            assert_eq!(decoded, nested);
        }
        assert_eq!(
            serde_json::to_string(&EvaluationError::FlagDisabled).unwrap(),
            "\"FLAG_DISABLED\""
        );
    }

    #[test]
    fn nondependency_bincode_encodings_remain_compatible() {
        // Golden bytes from the original derived EvaluationError representation.
        // Check both directions: round trips alone cannot detect discriminant changes.
        let cases: &[(EvaluationError, &[u8])] = &[
            (
                EvaluationError::TypeMismatch {
                    expected: ExpectedFlagType::Boolean,
                    found: FlagType::String,
                },
                &[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
            ),
            (EvaluationError::TargetingKeyMissing, &[1, 0, 0, 0]),
            (EvaluationError::InvalidContext, &[2, 0, 0, 0]),
            (EvaluationError::ConfigurationParseError, &[3, 0, 0, 0]),
            (EvaluationError::FlagConfigurationInvalid, &[4, 0, 0, 0]),
            (EvaluationError::ConfigurationMissing, &[5, 0, 0, 0]),
            (EvaluationError::FlagUnrecognizedOrDisabled, &[6, 0, 0, 0]),
            (EvaluationError::FlagDisabled, &[7, 0, 0, 0]),
            (EvaluationError::DefaultAllocationNull, &[8, 0, 0, 0]),
            (
                EvaluationError::Internal("owned".to_owned().into()),
                &[
                    9, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, b'o', b'w', b'n', b'e', b'd',
                ],
            ),
        ];
        for (error, bytes) in cases {
            assert_eq!(bincode::serialize(error).unwrap(), *bytes);
            assert_eq!(
                bincode::deserialize::<EvaluationError>(bytes).unwrap(),
                *error
            );
        }
    }

    #[test]
    fn deep_dependency_errors_drop_on_a_bounded_thread_stack() {
        for mut error in [
            EvaluationError::FlagDisabled,
            EvaluationError::Internal(String::from("owned terminal error").into()),
        ] {
            // Exceed the evaluator's depth cap to isolate recursive destruction from
            // platform-specific differences in the size of each cleanup frame.
            for _ in 0..10_000 {
                error = EvaluationError::DependencyEvaluationFailed {
                    flag_key: Str::from_static_str("prerequisite"),
                    source: error.into(),
                };
            }
            std::thread::Builder::new()
                .name("small-stack-error-drop".into())
                .stack_size(64 * 1024)
                .spawn(move || drop(error))
                .unwrap()
                .join()
                .unwrap();
        }
    }
}
