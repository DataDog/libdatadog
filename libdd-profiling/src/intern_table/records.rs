// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The record types: strings and functions.

use super::{Arena, Inline, Record, Storage, StringId};
use crate::collections::hash_table::Bucket;

/// UTF-8 strings, stored in an arena. Id 0 is the empty string.
pub struct Str(core::convert::Infallible);

impl Record for Str {
    type Key<'a> = &'a str;
    type Storage = Arena;
    const ZERO: &'static str = "";
    #[cfg(feature = "otel")]
    fn reserved(index: usize) -> Option<&'static str> {
        semconv::NAMES.get(index).copied()
    }

    fn encode(key: Self::Key<'_>) -> <Arena as Storage>::Item<'_> {
        key.as_bytes()
    }

    unsafe fn decode(item: <Arena as Storage>::Item<'_>) -> Self::Key<'_> {
        // SAFETY: the bytes came from `encode`, which takes a `&str`.
        unsafe { core::str::from_utf8_unchecked(item) }
    }
}

/// OpenTelemetry semantic-convention attribute names, interned by every
/// string table right after `""`, so their ids are the same in every table.
#[cfg(feature = "otel")]
pub mod semconv {
    use super::StringId;

    pub const THREAD_ID: StringId = StringId::from_index(1);
    pub const THREAD_NAME: StringId = StringId::from_index(2);
    pub const EXCEPTION_TYPE: StringId = StringId::from_index(3);
    pub const EXCEPTION_MESSAGE: StringId = StringId::from_index(4);

    /// The names, in id order starting at 1.
    pub const NAMES: [&str; 4] = [
        "thread.id",
        "thread.name",
        "exception.type",
        "exception.message",
    ];

    /// Each reserved id with its name.
    pub const IDS: [(StringId, &str); 4] = [
        (THREAD_ID, NAMES[0]),
        (THREAD_NAME, NAMES[1]),
        (EXCEPTION_TYPE, NAMES[2]),
        (EXCEPTION_MESSAGE, NAMES[3]),
    ];
}

/// A function, as in OTLP's `Function` message. Id 0 is all zeros.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Function {
    pub name: StringId,
    pub system_name: StringId,
    pub filename: StringId,
    /// OTLP's `start_line` is an `int64`. Line numbers fit in 32 bits, which
    /// keeps the record at 16 bytes with no padding.
    pub start_line: u32,
}

// SAFETY: four `u32`s (`Id` is a transparent `u32`): every bit pattern is a
// valid value, there is no padding, and the alignment is 4.
unsafe impl Bucket for Function {}

impl Record for Function {
    type Key<'a> = Self;
    type Storage = Inline<Self>;
    const ZERO: Self = Self {
        name: StringId::ZERO,
        system_name: StringId::ZERO,
        filename: StringId::ZERO,
        start_line: 0,
    };

    fn encode(key: Self::Key<'_>) -> <Inline<Self> as Storage>::Item<'_> {
        key
    }

    unsafe fn decode(item: <Inline<Self> as Storage>::Item<'_>) -> Self::Key<'_> {
        item
    }
}
