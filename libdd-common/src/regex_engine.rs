// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Workspace-wide regex engine re-exports.
//!
//! By default this module re-exports from the full [`regex`] crate with Unicode
//! support.
//! Enable the **`regex-ascii`** feature without default features to keep the
//! full regex engine and its performance optimizations while omitting Unicode
//! tables.
//! Enable the **`regex-lite`** feature to switch to [`regex_lite`] instead,
//! which trades advanced features (Unicode classes, look-around, etc.) for
//! smaller binary size and faster compile times.
//!
//! The **`regex-ascii`** and **`require-regex-full`** features force the full
//! `regex` crate even when `regex-lite` is enabled. The latter also enables
//! Unicode support for consumers that evaluate user-provided patterns.

#[cfg(all(
    feature = "regex-lite",
    not(any(feature = "regex-ascii", feature = "require-regex-full"))
))]
pub use regex_lite::{escape, Captures, Error, Regex, RegexBuilder, Replacer};

#[cfg(not(all(
    feature = "regex-lite",
    not(any(feature = "regex-ascii", feature = "require-regex-full"))
)))]
pub use regex::{escape, Captures, Error, Regex, RegexBuilder, Replacer};

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "regex-ascii", not(feature = "regex-unicode")))]
    #[test]
    fn ascii_full_engine_supports_set_operations() {
        use super::Regex;

        assert!(Regex::new(r"[a-z&&[^aeiou]]+").unwrap().is_match("rhythm"));
    }

    #[cfg(all(
        feature = "regex-unicode",
        any(
            not(feature = "regex-lite"),
            feature = "regex-ascii",
            feature = "require-regex-full"
        )
    ))]
    #[test]
    fn unicode_engine_includes_unicode_tables() {
        use super::Regex;

        assert!(Regex::new(r"\p{Greek}+").unwrap().is_match("Δοκιμή"));
    }
}
