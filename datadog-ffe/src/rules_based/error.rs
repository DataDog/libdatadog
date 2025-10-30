// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

use crate::rules_based::ufc::VariationType;

/// Enum representing possible errors that can occur during evaluation.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvaluationError {
    /// Requested flag has invalid type.
    #[error("invalid flag type (expected: {expected:?}, found: {found:?})")]
    TypeMismatch {
        /// Expected type of the flag.
        expected: VariationType,
        /// Actual type of the flag.
        found: VariationType,
    },

    /// Configuration received from the server is invalid for the SDK. This should normally never
    /// happen and is likely a signal that you should update SDK.
    #[error("unexpected configuration received from the server")]
    UnexpectedConfigurationError,
}

/// Enum representing all possible reasons that could result in evaluation returning an error or
/// default assignment.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvaluationFailure {
    /// True evaluation error that should be returned to the user.
    #[error(transparent)]
    Error(EvaluationError),

    /// Configuration has not been fetched yet.
    #[error("configuration has not been fetched yet")]
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
}

impl From<EvaluationError> for EvaluationFailure {
    fn from(value: EvaluationError) -> EvaluationFailure {
        EvaluationFailure::Error(value)
    }
}
