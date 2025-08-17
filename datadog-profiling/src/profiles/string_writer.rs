// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt::{self, Write};

/// A `fmt::Write` adapter that grows a `String` using `try_reserve` before
/// each write, returning `fmt::Error` on allocation failure.
#[derive(Debug, Default)]
pub struct FallibleStringWriter {
    buf: String,
}

impl FallibleStringWriter {
    /// Formats `value` into a newly allocated `String`, pre-reserving zero
    /// additional capacity. Returns `fmt::Error` if allocation fails.
    pub fn try_format<T: fmt::Display>(value: &T) -> Result<String, fmt::Error> {
        Self::try_format_with_size_hint(value, 0)
    }

    /// Formats `value` into a newly allocated `String`, attempting to reserve
    /// `capacity` bytes up-front, and reserving incrementally thereafter.
    /// Returns `fmt::Error` if allocation fails.
    pub fn try_format_with_size_hint<T: fmt::Display>(
        value: &T,
        capacity: usize,
    ) -> Result<String, fmt::Error> {
        let mut w = FallibleStringWriter { buf: String::new() };
        let _ = w.buf.try_reserve(capacity);
        write!(&mut w, "{}", value)?;
        Ok(w.buf)
    }
}

impl Write for FallibleStringWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.buf.try_reserve(s.len()).map_err(|_| fmt::Error)?;
        self.buf.push_str(s);
        Ok(())
    }
}
