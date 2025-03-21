// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Error types for `metrics` module

/// Errors for the function [`crate::metric::Metric::parse`]
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    /// Parse failure given in text
    #[error("parse failure: {0}")]
    Raw(String),
    #[error("unsupported metric type: {0}")]
    UnsupportedType(String),
}

/// Failure to create a new `Aggregator`
#[derive(Debug, thiserror::Error, Clone, Copy)]
pub enum Creation {
    /// The specified context max is too large given our constants. Indicates a
    /// serious programming error.
    #[error("context max is too large")]
    Contexts,
}

/// Failures from `Aggregator::insert`
#[derive(Debug, thiserror::Error)]
pub enum Insert {
    /// The current interval is full and no further metrics can be inserted. The
    /// inserted metric is returned.
    #[error("interval is full")]
    Overflow,
    /// Unable to parse passed values
    #[error(transparent)]
    ValuesIteration(#[from] std::num::ParseFloatError),
}
