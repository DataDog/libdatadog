// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Workspace-wide regex engine re-exports.
//!
//! By default this crate re-exports from the full `regex` crate. Enable the
//! **`regex-lite`** feature to switch to `regex_lite` instead, which trades
//! Unicode character classes and other advanced features for a smaller binary
//! size and faster compile times.
//!
//! The **`require-regex-full`** feature forces the full `regex` crate even when
//! `regex-lite` is enabled, for consumers that evaluate user-provided regexes
//! requiring Unicode character class support.
//!
//! # Examples
//!
//! ```
//! use libdd_regex::Regex;
//!
//! let re = Regex::new(r"\d+").unwrap();
//! assert!(re.is_match("answer: 42"));
//! ```

// With neither `full` nor `regex-lite` the crate exports nothing: a dependency on it needs one of
// them (the default, `full`, or `regex-lite`) to get `Regex`.

#[cfg(all(feature = "regex-lite", not(feature = "require-regex-full")))]
pub use regex_lite::{Captures, Error, Regex, RegexBuilder, Replacer, escape};

#[cfg(all(
    feature = "full",
    not(all(feature = "regex-lite", not(feature = "require-regex-full")))
))]
pub use regex::{Captures, Error, Regex, RegexBuilder, Replacer, escape};

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "full", feature = "regex-lite"))]
    use super::Regex;

    #[cfg(all(feature = "regex-lite", not(feature = "require-regex-full")))]
    #[test]
    fn lite_engine_matches() {
        assert!(Regex::new(r"[0-9]+").unwrap().is_match("abc123"));
    }

    #[cfg(all(
        feature = "full",
        not(all(feature = "regex-lite", not(feature = "require-regex-full")))
    ))]
    #[test]
    fn full_engine_supports_set_operations() {
        assert!(Regex::new(r"[a-z&&[^aeiou]]+").unwrap().is_match("rhythm"));
    }

    #[cfg(all(
        feature = "full",
        not(all(feature = "regex-lite", not(feature = "require-regex-full")))
    ))]
    #[test]
    fn full_engine_includes_unicode_tables() {
        assert!(Regex::new(r"\p{Greek}+").unwrap().is_match("Δοκιμή"));
    }

    #[cfg(any(feature = "full", feature = "regex-lite"))]
    #[test]
    fn escape_quotes_metacharacters() {
        assert_eq!(super::escape("a.b*c"), r"a\.b\*c");
    }
}
