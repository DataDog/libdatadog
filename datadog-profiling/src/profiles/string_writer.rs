// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt::{self, Write};
use std::collections::TryReserveError;

/// A `fmt::Write` adapter that grows a `String` using `try_reserve` before
/// each write, returning `fmt::Error` on allocation failure.
#[derive(Debug)]
pub struct FallibleStringWriter {
    buf: String,
}

impl Default for FallibleStringWriter {
    fn default() -> FallibleStringWriter {
        FallibleStringWriter::new()
    }
}

impl FallibleStringWriter {
    /// Creates a new empty string writer.
    pub const fn new() -> Self {
        Self { buf: String::new() }
    }

    /// Creates a new fallible string writer with a previously existing string
    /// as the start of the buffer. New writes will append to the end of this.
    pub const fn new_from_existing(buf: String) -> FallibleStringWriter {
        FallibleStringWriter { buf }
    }

    /// Tries to reserve capacity for at least additional bytes more than the
    /// current length. The allocator may reserve more space to speculatively
    /// avoid frequent allocations.
    pub fn try_reserve(&mut self, len: usize) -> Result<(), TryReserveError> {
        self.buf.try_reserve(len)
    }

    /// Tries to reserve the minimum capacity for at least `additional` bytes
    /// more than the current length. Unlike [`try_reserve`], this will not
    /// deliberately over-allocate to speculatively avoid frequent allocations.
    ///
    /// Note that the allocator may give the collection more space than it
    /// requests. Therefore, capacity can not be relied upon to be precisely
    /// minimal. Prefer [`try_reserve`] if future insertions are expected.
    pub fn try_reserve_exact(&mut self, len: usize) -> Result<(), TryReserveError> {
        self.buf.try_reserve_exact(len)
    }

    pub fn try_push_str(&mut self, str: &str) -> Result<(), TryReserveError> {
        self.try_reserve(str.len())?;
        self.buf.push_str(str);
        Ok(())
    }
}

impl From<FallibleStringWriter> for String {
    fn from(w: FallibleStringWriter) -> String {
        w.buf
    }
}

impl From<String> for FallibleStringWriter {
    fn from(buf: String) -> FallibleStringWriter {
        FallibleStringWriter { buf }
    }
}

impl Write for FallibleStringWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.buf.try_reserve(s.len()).map_err(|_| fmt::Error)?;
        self.buf.push_str(s);
        Ok(())
    }
}
