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
        self.try_push_str(s).map_err(|_| fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn test_new_and_default() {
        let writer = FallibleStringWriter::new();
        let s: String = writer.into();
        assert_eq!(s, "");

        let writer = FallibleStringWriter::default();
        let s: String = writer.into();
        assert_eq!(s, "");
    }

    #[test]
    fn test_write_str() {
        let mut writer = FallibleStringWriter::new();
        writer.write_str("Hello").unwrap();
        writer.write_str(", ").unwrap();
        writer.write_str("World!").unwrap();

        let s: String = writer.into();
        assert_eq!(s, "Hello, World!");
    }

    #[test]
    fn test_write_formatted() {
        let mut writer = FallibleStringWriter::new();
        write!(writer, "x = {}, ", 10).unwrap();
        write!(writer, "y = {}, ", 20).unwrap();
        write!(writer, "sum = {}", 10 + 20).unwrap();

        let s: String = writer.into();
        assert_eq!(s, "x = 10, y = 20, sum = 30");
    }

    #[test]
    fn test_try_push_str() {
        let mut writer = FallibleStringWriter::new();
        writer.try_push_str("Hello").unwrap();
        writer.try_push_str(" ").unwrap();
        writer.try_push_str("World").unwrap();

        let s: String = writer.into();
        assert_eq!(s, "Hello World");
    }

    #[test]
    fn test_try_reserve() {
        // Marcus Aurelius, Meditations (public domain)
        let strings = [
            "The happiness of your life depends upon the quality of your thoughts: ",
            "therefore, guard accordingly, and take care that you entertain ",
            "no notions unsuitable to virtue and reasonable nature.",
        ];
        let total_len: usize = strings.iter().map(|s| s.len()).sum();

        let mut writer = FallibleStringWriter::new();
        // Asking for more than is needed just to ensure that the test isn't
        // accidentally correct.
        let capacity = 2 * total_len + 7;
        writer.try_reserve_exact(capacity).unwrap();

        // After reserving, we should be able to write all strings (and more).
        for s in &strings {
            writer.write_str(s).unwrap();
        }

        let result: String = writer.into();
        assert_eq!(result, strings.join(""));

        // It can't be less, but an allocator is free to round, even on a
        // try_reserve_exact.
        assert!(result.capacity() >= capacity);
    }

    #[test]
    fn test_from_existing_string() {
        // Test From<String>, new_from_existing, and appending
        let s = String::from("start: ");
        let mut writer = FallibleStringWriter::from(s);
        write!(writer, "{}", 123).unwrap();
        assert_eq!(String::from(writer), "start: 123");

        // Test new_from_existing
        let mut writer = FallibleStringWriter::new_from_existing(String::from("prefix-"));
        writer.try_push_str("suffix").unwrap();
        assert_eq!(String::from(writer), "prefix-suffix");
    }

    #[test]
    fn test_write_unicode() {
        let mut writer = FallibleStringWriter::new();
        write!(writer, "Hello 👋 World 🌍").unwrap();

        let s: String = writer.into();
        assert_eq!(s, "Hello 👋 World 🌍");
    }

    #[test]
    fn test_write_long_string() {
        let mut writer = FallibleStringWriter::new();
        let long_str = "a".repeat(1000);

        writer.write_str(&long_str).unwrap();

        let s: String = writer.into();
        assert_eq!(s.len(), 1000);
        assert_eq!(s, long_str);
    }
}
