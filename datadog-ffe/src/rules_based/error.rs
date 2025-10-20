use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rules_based::ufc::VariationType;

/// Represents a result type for operations in the Eppo SDK.
///
/// This type alias is used throughout the SDK to indicate the result of operations that may return
/// errors specific to the Eppo SDK.
///
/// This `Result` type is a standard Rust `Result` type where the error variant is defined by the
/// eppo-specific [`Error`] enum.
pub type Result<T> = std::result::Result<T, Error>;

/// Enum representing possible errors that can occur in the Eppo SDK.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
    /// Error evaluating a flag.
    #[error(transparent)]
    EvaluationError(EvaluationError),

    /// Invalid base URL configuration.
    #[error("invalid base_url configuration")]
    InvalidBaseUrl(#[source] url::ParseError),

    /// The request was unauthorized, possibly due to an invalid API key.
    #[error("unauthorized, api_key is likely invalid")]
    Unauthorized,

    /// Indicates that the poller thread panicked. This should normally never happen.
    #[error("poller thread panicked")]
    PollerThreadPanicked,

    /// An I/O error.
    #[error(transparent)]
    // std::io::Error is not clonable, so we're wrapping it in an Arc.
    Io(Arc<std::io::Error>),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(Arc::new(value))
    }
}

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
    #[error("unexpected configuration received from the server, try upgrading Eppo SDK")]
    UnexpectedConfigurationError,

    /// An error occurred while parsing the configuration (server sent unexpected response). It is
    /// recommended to upgrade the Eppo SDK.
    #[error("error parsing configuration, try upgrading Eppo SDK")]
    UnexpectedConfigurationParseError,
}

/// Enum representing all possible reasons that could result in evaluation returning an error or
/// default assignment.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum EvaluationFailure {
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

    #[error("flag resolved to a non-bandit variation")]
    NonBanditVariation,

    #[error("no actions were supplied to bandit evaluation")]
    NoActionsSuppliedForBandit,
}
