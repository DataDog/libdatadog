// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Error types for span inference.

use std::fmt;

/// Errors that can occur during span inference.
#[derive(Debug)]
pub enum InferrerError {
    /// The payload could not be parsed as valid JSON.
    InvalidJson(serde_json::Error),
    /// The payload did not match any known trigger type.
    UnknownPayload,
}

impl fmt::Display for InferrerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InferrerError::InvalidJson(e) => write!(f, "invalid JSON payload: {e}"),
            InferrerError::UnknownPayload => write!(f, "payload did not match any known trigger"),
        }
    }
}

impl std::error::Error for InferrerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InferrerError::InvalidJson(e) => Some(e),
            InferrerError::UnknownPayload => None,
        }
    }
}

impl From<serde_json::Error> for InferrerError {
    fn from(e: serde_json::Error) -> Self {
        InferrerError::InvalidJson(e)
    }
}
