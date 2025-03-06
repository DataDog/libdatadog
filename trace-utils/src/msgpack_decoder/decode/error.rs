// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    InvalidConversion(String),
    InvalidType(String),
    InvalidFormat(String),
    IOError,
    Utf8Error(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidConversion(msg) => write!(f, "Failed to convert value: {}", msg),
            DecodeError::IOError => write!(f, "Failed to read from buffer"),
            DecodeError::InvalidType(msg) => write!(f, "Invalid type encountered: {}", msg),
            DecodeError::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
            DecodeError::Utf8Error(msg) => write!(f, "Failed to read utf8 value: {}", msg),
        }
    }
}
