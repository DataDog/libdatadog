// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;

/// Represent error that can happen while using the tracer flare.
#[derive(Debug, PartialEq)]
pub enum FlareError {
    /// Listening to the RemoteConfig failed.
    ListeningError(String),
    /// Parsing of config failed.
    ParsingError(String),
    /// Sending the flare failed.
    SendError(String),
    /// Creating the zipped flare failed.
    ZipError(String),
}

impl std::fmt::Display for FlareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlareError::ListeningError(msg) => write!(f, "Listening failed with: {msg}"),
            FlareError::ParsingError(msg) => write!(f, "Parsing failed with: {msg}"),
            FlareError::SendError(msg) => write!(f, "Sending the flare failed with: {msg}"),
            FlareError::ZipError(msg) => write!(f, "Creating the zip failed with: {msg}"),
        }
    }
}

impl Error for FlareError {}
