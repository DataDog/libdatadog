// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provides error definitions for the Telemetry module.
use std::error::Error;
use std::fmt::Display;

/// TelemetryError holds different types of errors that occur when sending metrics.
#[derive(Debug)]
pub enum TelemetryError {
    /// Invalid configuration during Telemetry client creation.
    Builder(String),
    /// Error while sending metrics.
    Send(String),
}

impl Display for TelemetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryError::Builder(e) => write!(f, "Telemetry client builder failed: {}", e),
            TelemetryError::Send(e) => write!(f, "Send metric failed: {}", e),
        }
    }
}

impl From<anyhow::Error> for TelemetryError {
    fn from(value: anyhow::Error) -> Self {
        TelemetryError::Send(value.to_string())
    }
}

impl Error for TelemetryError {}
