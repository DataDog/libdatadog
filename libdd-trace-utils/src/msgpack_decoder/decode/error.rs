// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Represent error that can happen while decoding msgpack.
#[derive(Debug, PartialEq)]
pub enum DecodeError {
    /// Failed to convert a number to the expected type.
    InvalidConversion(String),
    /// Payload does not match the expected type for a trace payload.
    InvalidType(String),
    /// Payload is not a valid msgpack object.
    InvalidFormat(String),
    /// Failed to read the buffer.
    IOError,
    /// The payload contains non-utf8 strings.
    Utf8Error(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidConversion(msg) => write!(f, "Failed to convert value: {msg}"),
            DecodeError::InvalidType(msg) => write!(f, "Invalid type encountered: {msg}"),
            DecodeError::InvalidFormat(msg) => write!(f, "Invalid format: {msg}"),
            DecodeError::IOError => write!(f, "Failed to read from buffer"),
            DecodeError::Utf8Error(msg) => write!(f, "Failed to read utf8 value: {msg}"),
        }
    }
}
