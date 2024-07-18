// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    WrongConversion,
    WrongType,
    WrongFormat,
    IOError,
    Utf8Error,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::WrongConversion => write!(f, "Failed to cast value"),
            DecodeError::IOError => write!(f, "Failed to read from buffer"),
            DecodeError::WrongType => write!(f, "Invalid type encountered"),
            DecodeError::WrongFormat => write!(f, "Invalid format"),
            DecodeError::Utf8Error => write!(f, "Failed to read utf8 value"),
        }
    }
}
