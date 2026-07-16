// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Workspace-wide regex engine re-exports.
//!
//! By default this module re-exports from the full [`regex`] crate.
//! Enable the **`regex-lite`** feature to switch to [`regex_lite`] instead,
//! which trades advanced features (Unicode classes, look-around, etc.) for
//! smaller binary size and faster compile times.
//!
//! The **`require-regex-full`** feature forces the full `regex` crate even
//! when `regex-lite` is enabled, for consumers that evaluate user-provided
//! regexes requiring Unicode character class support.

#[cfg(all(feature = "regex-lite", not(feature = "require-regex-full")))]
pub use regex_lite::{escape, Captures, Error, Regex, RegexBuilder, Replacer};

#[cfg(not(all(feature = "regex-lite", not(feature = "require-regex-full"))))]
pub use regex::{escape, Captures, Error, Regex, RegexBuilder, Replacer};
