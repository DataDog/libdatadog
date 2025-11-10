// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

use crate::rules_based::{ExpectedFlagType, FlagType};

/// Enum representing all possible reasons that could result in evaluation returning an error or
/// default assignment.
///
/// Not all of these are technically "errors"—some can be expected to occur frequently (e.g.,
/// `FlagDisabled` or `DefaultAllocation`).
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum EvaluationError {
    /// Requested flag has unexpected type.
    #[error("invalid flag type (expected: {expected:?}, found: {found:?})")]
    TypeMismatch {
        /// Expected type of the flag.
        expected: ExpectedFlagType,
        /// Actual type of the flag.
        found: FlagType,
    },

    /// Failed to parse configuration. This should normally never happen and is likely a signal
    /// that you should update SDK.
    #[error("failed to parse configuration")]
    ConfigurationParseError,

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
}
