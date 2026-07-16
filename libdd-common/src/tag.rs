// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::{
    borrow::Cow,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::fmt::{self, Debug, Display, Formatter};

pub use static_assertions::{const_assert, const_assert_ne};

use serde::{Deserialize, Serialize};

/// A validated Datadog tag.
#[allow(clippy::unsafe_derive_deserialize)]
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag {
    /// Many tags are made from literal strings, such as:
    ///  - `language:native`
    ///  - `src_library:libdatadog`
    ///  - `type:timeout`
    ///
    /// So being able to save allocations is nice.
    value: Cow<'static, str>,
}

impl Tag {
    /// Used by the [`tag!`] macro.
    ///
    /// # Safety
    ///
    /// Callers must uphold the validation invariants enforced by [`tag!`].
    #[must_use]
    pub const unsafe fn from_static_unchecked(value: &'static str) -> Self {
        Self {
            value: Cow::Borrowed(value),
        }
    }
}

/// Creates a tag from a key and value known at compile time.
///
/// Invalid literal tags fail compilation. For dynamic values, use [`Tag::new`].
#[macro_export]
macro_rules! tag {
    ($key:expr, $val:expr) => {{
        $crate::tag::const_assert!(!$val.is_empty());

        const COMBINED: &'static str = $crate::const_format::concatcp!($key, ":", $val);

        $crate::tag::const_assert!(COMBINED.as_bytes()[0].is_ascii_alphabetic());
        $crate::tag::const_assert!(COMBINED.as_bytes().len() <= 200);

        #[allow(unused_unsafe)]
        let tag = unsafe { $crate::tag::Tag::from_static_unchecked(COMBINED) };
        tag
    }};
}

impl Debug for Tag {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tag")
            .field("value", &self.value)
            .finish()
    }
}

impl AsRef<str> for Tag {
    fn as_ref(&self) -> &str {
        self.value.as_ref()
    }
}

impl Display for Tag {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.value)
    }
}

impl Tag {
    fn from_value<'a, IntoCow>(chunk: IntoCow) -> anyhow::Result<Self>
    where
        IntoCow: Into<Cow<'a, str>>,
    {
        let chunk = chunk.into();

        anyhow::ensure!(!chunk.is_empty(), "tag is empty");

        let mut chars = chunk.chars();
        anyhow::ensure!(
            chars.next() != Some(':'),
            "tag '{chunk}' begins with a colon"
        );
        anyhow::ensure!(chars.last() != Some(':'), "tag '{chunk}' ends with a colon");

        Ok(Self {
            value: Cow::Owned(chunk.into_owned()),
        })
    }

    /// Creates a tag from a dynamic key and value.
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting tag is empty, begins with a colon,
    /// or ends with a colon.
    pub fn new<K, V>(key: K, value: V) -> anyhow::Result<Self>
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        Self::from_value(format!("{}:{}", key.as_ref(), value.as_ref()))
    }
}

/// Parses comma- or space-separated tags.
///
/// Returns valid tags and an optional message describing invalid entries.
pub fn parse_tags(value: &str) -> (Vec<Tag>, Option<String>) {
    let chunks = value
        .split(&[',', ' '][..])
        .filter(|value| !value.is_empty())
        .map(Tag::from_value);

    let mut tags = vec![];
    let mut error_message = String::new();
    for result in chunks {
        match result {
            Ok(tag) => tags.push(tag),
            Err(error) => {
                if error_message.is_empty() {
                    error_message += "Errors while parsing tags: ";
                } else {
                    error_message += ", ";
                }
                error_message += &error.to_string();
            }
        }
    }

    let error_message = if error_message.is_empty() {
        None
    } else {
        Some(error_message)
    };
    (tags, error_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_tag_is_send() {
        fn is_send<T: Send>(_value: T) {}
        is_send(tag!("src_library", "libdatadog"));
    }

    #[test]
    fn rejects_empty_values() {
        assert!(Tag::new("", "value").is_err());
        assert!(Tag::new("key", "").is_err());
    }

    #[test]
    fn permits_colons_in_values() {
        let tag = Tag::new("env", "staging:east").unwrap();
        assert_eq!(tag.to_string(), "env:staging:east");
    }

    #[test]
    fn parses_tag_lists() {
        let (tags, error) = parse_tags("env:staging location:nyc");
        assert_eq!(
            tags,
            vec![
                Tag::new("env", "staging").unwrap(),
                Tag::new("location", "nyc").unwrap(),
            ]
        );
        assert!(error.is_none());
    }

    #[test]
    fn reports_invalid_entries() {
        let (tags, error) = parse_tags(":invalid,valid:value");
        assert_eq!(tags, vec![Tag::new("valid", "value").unwrap()]);
        assert!(error.is_some());
    }

    #[test]
    fn accepts_lossy_utf8() {
        let bytes = &[b'a', 0b1111_0111];
        let key = String::from_utf8_lossy(bytes);
        let tag = Tag::new(key, "value").unwrap();
        assert_eq!(tag.to_string(), "a\u{FFFD}:value");
    }

    #[test]
    fn permits_unkeyed_tags() {
        let tag = Tag::from_value("value").unwrap();
        assert_eq!(tag.to_string(), "value");
    }

    #[test]
    fn rejects_boundary_colons() {
        assert!(Tag::from_value(":value").is_err());
        assert!(Tag::from_value("value:").is_err());
    }

    #[test]
    fn retains_permissive_dynamic_validation() {
        let cases = [
            ("_begins_with_non-letter".to_string(), "value"),
            ("the-tag-length-is-over-200-characters".repeat(6), "value"),
        ];

        for (key, value) in cases {
            assert!(Tag::new(key, value).is_ok());
        }
    }
}
