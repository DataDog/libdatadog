// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::BuildError;

/// A borrowed HTTP header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<'a> {
    name: &'a str,
    value: &'a str,
}

impl<'a> Header<'a> {
    /// Creates a header after validating its name and value.
    ///
    /// Header names must use the HTTP token grammar. Header values are restricted to visible
    /// ASCII plus horizontal tab to prevent CRLF injection and keep the signal-safe formatter
    /// small.
    pub fn new(name: &'a str, value: &'a str) -> Result<Self, BuildError> {
        let header = Self { name, value };
        header.validate()?;
        Ok(header)
    }

    /// Creates a header without immediate validation.
    ///
    /// The request writer still validates every header before emitting bytes, so this constructor
    /// is useful for static protocol headers without making invalid data silently writable.
    pub const fn new_unchecked(name: &'a str, value: &'a str) -> Self {
        Self { name, value }
    }

    /// Returns the header name.
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// Returns the header value.
    pub const fn value(&self) -> &'a str {
        self.value
    }

    pub(crate) fn validate(&self) -> Result<(), BuildError> {
        validate_header_name(self.name)?;
        validate_header_value(self.value)
    }
}

pub(crate) fn validate_header_name(name: &str) -> Result<(), BuildError> {
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        return Err(BuildError::InvalidHeaderName);
    }
    Ok(())
}

pub(crate) fn validate_header_value(value: &str) -> Result<(), BuildError> {
    if !value.bytes().all(is_header_value_byte) {
        return Err(BuildError::InvalidHeaderValue);
    }
    Ok(())
}

fn is_token_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
    )
}

fn is_header_value_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | 0x20..=0x7e)
}
