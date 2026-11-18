// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The error a call returns: a stable code and a message.

use std::ffi::{CStr, c_char};

use libdd_ai_usage::{ErrorCode, MetricError};
use libdd_common_ffi::Error;

/// Why a call failed. [`AiUsageErrorCode::Internal`] is a failure of the
/// call itself; every other code is a rejection of its input.
///
/// The values are stable: a code is never renumbered, and a new one gets a
/// new value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiUsageErrorCode {
    /// The call failed: a null pointer, a null or released handle, text that
    /// is not UTF-8 where only text is read, or a panic that was caught.
    Internal = 0,
    /// The input names nothing in the vocabulary: a provider name that forms
    /// no identifier, a route that is no operation, or a failure with no
    /// `error.type`. The message says which.
    NoMatch = 1,
    // The rejection codes of the specification
    // (`PORTABLE-METRICS.md#error-codes`): the input broke one of its rules.
    // `ddog_ai_usage_error_code_name` gives each its stable name, such as
    // `field_type_invalid`.
    FieldTypeInvalid = 2,
    ValueNotAllowed = 3,
    IdentifierInvalid = 4,
    CountInvalid = 5,
    DurationInvalid = 6,
    RequiredFieldMissing = 7,
    FieldsContradict = 8,
    SequenceInvalid = 9,
    EventOrderInvalid = 10,
    TimestampInvalid = 11,
    DeploymentAttributeUnknown = 12,
    ContextBandsInvalid = 13,
    ProfileInvalid = 14,
    MetricProfileMismatch = 15,
}

impl From<ErrorCode> for AiUsageErrorCode {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::FieldTypeInvalid => Self::FieldTypeInvalid,
            ErrorCode::ValueNotAllowed => Self::ValueNotAllowed,
            ErrorCode::IdentifierInvalid => Self::IdentifierInvalid,
            ErrorCode::CountInvalid => Self::CountInvalid,
            ErrorCode::DurationInvalid => Self::DurationInvalid,
            ErrorCode::RequiredFieldMissing => Self::RequiredFieldMissing,
            ErrorCode::FieldsContradict => Self::FieldsContradict,
            ErrorCode::SequenceInvalid => Self::SequenceInvalid,
            ErrorCode::EventOrderInvalid => Self::EventOrderInvalid,
            ErrorCode::TimestampInvalid => Self::TimestampInvalid,
            ErrorCode::DeploymentAttributeUnknown => Self::DeploymentAttributeUnknown,
            ErrorCode::ContextBandsInvalid => Self::ContextBandsInvalid,
            ErrorCode::ProfileInvalid => Self::ProfileInvalid,
            ErrorCode::MetricProfileMismatch => Self::MetricProfileMismatch,
        }
    }
}

impl AiUsageErrorCode {
    /// The stable name of the code: the specification's for a rejection,
    /// `internal` or `no_match` otherwise.
    pub fn name(self) -> &'static CStr {
        match self {
            Self::Internal => c"internal",
            Self::NoMatch => c"no_match",
            Self::FieldTypeInvalid => c"field_type_invalid",
            Self::ValueNotAllowed => c"value_not_allowed",
            Self::IdentifierInvalid => c"identifier_invalid",
            Self::CountInvalid => c"count_invalid",
            Self::DurationInvalid => c"duration_invalid",
            Self::RequiredFieldMissing => c"required_field_missing",
            Self::FieldsContradict => c"fields_contradict",
            Self::SequenceInvalid => c"sequence_invalid",
            Self::EventOrderInvalid => c"event_order_invalid",
            Self::TimestampInvalid => c"timestamp_invalid",
            Self::DeploymentAttributeUnknown => c"deployment_attribute_unknown",
            Self::ContextBandsInvalid => c"context_bands_invalid",
            Self::ProfileInvalid => c"profile_invalid",
            Self::MetricProfileMismatch => c"metric_profile_mismatch",
        }
    }
}

/// The error of a failed call, which returns null on success. Release it
/// with [`ddog_ai_usage_error_free`].
#[repr(C)]
#[derive(Debug)]
pub struct AiUsageError {
    pub code: AiUsageErrorCode,
    /// What failed, for a log: read it with `ddog_Error_message`. A
    /// rejection's message is the one ABI v1 wrote in its
    /// `{"error": {"code", "message"}}`.
    pub message: Error,
}

/// How a call failed, before it crosses the boundary.
pub(crate) enum Failure {
    Rejected(MetricError),
    NoMatch(&'static str),
    Internal(&'static str),
}

impl From<MetricError> for Failure {
    fn from(error: MetricError) -> Self {
        Failure::Rejected(error)
    }
}

impl From<Failure> for Box<AiUsageError> {
    fn from(failure: Failure) -> Self {
        let (code, message) = match failure {
            Failure::Rejected(error) => (error.code().into(), error.message().to_string()),
            Failure::NoMatch(message) => (AiUsageErrorCode::NoMatch, message.to_string()),
            Failure::Internal(message) => (AiUsageErrorCode::Internal, message.to_string()),
        };
        Box::new(AiUsageError {
            code,
            message: Error::from(message),
        })
    }
}

/// The error of a call, or none.
pub(crate) fn error_of<T>(result: Result<T, Failure>) -> Option<Box<AiUsageError>> {
    result.err().map(Into::into)
}

/// The error of a call that panicked.
pub(crate) fn panicked() -> Option<Box<AiUsageError>> {
    Some(Failure::Internal("the call panicked").into())
}

/// Release an error a call returned. A null error is a no-op.
// allow(ffi-panic-boundary): only drops a box holding a `ddog_Error`, which cannot panic.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_ai_usage_error_free(error: Option<Box<AiUsageError>>) {
    drop(error);
}

/// The stable name of `error`'s code, such as `field_type_invalid`
/// ([`AiUsageErrorCode`]): a NUL-terminated string this library owns, valid
/// for as long as the library is loaded. A null error names nothing (`""`).
// allow(ffi-panic-boundary): reads one field and returns a static string; it cannot panic.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_ai_usage_error_code_name(error: Option<&AiUsageError>) -> *const c_char {
    error.map_or(c"", |error| error.code.name()).as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rejection_code_has_the_specification_name() {
        for code in ErrorCode::ALL {
            assert_eq!(
                AiUsageErrorCode::from(code).name().to_str().unwrap(),
                code.as_str()
            );
        }
    }

    #[test]
    fn a_failure_becomes_an_error() {
        let error: Box<AiUsageError> = Failure::Rejected(MetricError::new(
            ErrorCode::ProfileInvalid,
            "unknown profile",
        ))
        .into();
        assert_eq!(error.code, AiUsageErrorCode::ProfileInvalid);
        assert_eq!(error.message.to_string(), "unknown profile");
        assert_eq!(
            // SAFETY: a static NUL-terminated string.
            unsafe { CStr::from_ptr(ddog_ai_usage_error_code_name(Some(&error))) },
            c"profile_invalid"
        );
        ddog_ai_usage_error_free(Some(error));
        // SAFETY: a static NUL-terminated string.
        assert_eq!(
            unsafe { CStr::from_ptr(ddog_ai_usage_error_code_name(None)) },
            c""
        );
        let error = panicked().unwrap();
        assert_eq!(error.code, AiUsageErrorCode::Internal);
        ddog_ai_usage_error_free(Some(error));
    }
}
