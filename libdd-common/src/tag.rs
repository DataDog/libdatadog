// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "alloc")]
use alloc::{
    borrow::Cow,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
#[cfg(feature = "alloc")]
use core::fmt::Debug;
use core::fmt::{self, Display, Formatter};

#[cfg(feature = "alloc")]
pub use static_assertions::{const_assert, const_assert_ne};

#[cfg(feature = "alloc")]
use serde::Deserialize;
use serde::{Serialize, Serializer};

/// A borrowed validated Datadog tag.
///
/// The key and value can be borrowed separately, avoiding the allocation that
/// would otherwise be needed to join them with a colon.
#[derive(Clone, Copy, Debug)]
pub struct TagRef<'a> {
    key: Option<&'a str>,
    value: &'a str,
}

impl<'a> TagRef<'a> {
    /// Creates a borrowed tag from separate key and value strings.
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting tag would begin or end with a
    /// colon.
    pub fn new(key: &'a str, value: &'a str) -> Result<Self, TagError> {
        if key.is_empty() || key.starts_with(':') {
            return Err(TagError::BeginsWithColon);
        }
        if value.is_empty() || value.ends_with(':') {
            return Err(TagError::EndsWithColon);
        }

        Ok(Self {
            key: Some(key),
            value,
        })
    }

    /// Creates a borrowed tag from an already formatted value.
    ///
    /// Both keyed (`key:value`) and unkeyed (`value`) tags are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag is empty, begins with a colon, or ends
    /// with a colon.
    pub fn from_value(value: &'a str) -> Result<Self, TagError> {
        validate_value(value)?;
        Ok(Self::from_valid_value(value))
    }

    fn from_valid_value(value: &'a str) -> Self {
        match value.split_once(':') {
            Some((key, value)) => Self {
                key: Some(key),
                value,
            },
            None => Self { key: None, value },
        }
    }

    /// Returns the borrowed key and value components.
    ///
    /// Unkeyed tags return `None` for the key.
    pub const fn parts(self) -> (Option<&'a str>, &'a str) {
        (self.key, self.value)
    }
}

/// Validation failure for a borrowed tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagError {
    /// The tag is empty.
    Empty,
    /// The tag begins with a colon.
    BeginsWithColon,
    /// The tag ends with a colon.
    EndsWithColon,
}

impl Display for TagError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "tag is empty",
            Self::BeginsWithColon => "tag begins with a colon",
            Self::EndsWithColon => "tag ends with a colon",
        })
    }
}

impl core::error::Error for TagError {}

impl Display for TagRef<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if let Some(key) = self.key {
            formatter.write_str(key)?;
            formatter.write_str(":")?;
        }
        formatter.write_str(self.value)
    }
}

impl Serialize for TagRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

fn validate_value(value: &str) -> Result<(), TagError> {
    if value.is_empty() {
        return Err(TagError::Empty);
    }
    if value.starts_with(':') {
        return Err(TagError::BeginsWithColon);
    }
    if value.ends_with(':') {
        return Err(TagError::EndsWithColon);
    }
    Ok(())
}

/// A validated Datadog tag.
#[cfg(feature = "alloc")]
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

#[cfg(feature = "alloc")]
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
#[cfg(feature = "alloc")]
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

#[cfg(feature = "alloc")]
impl Debug for Tag {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tag")
            .field("value", &self.value)
            .finish()
    }
}

#[cfg(feature = "alloc")]
impl AsRef<str> for Tag {
    fn as_ref(&self) -> &str {
        self.value.as_ref()
    }
}

#[cfg(feature = "alloc")]
impl Display for Tag {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.value)
    }
}

#[cfg(feature = "alloc")]
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

#[cfg(feature = "alloc")]
impl<'a> From<&'a Tag> for TagRef<'a> {
    fn from(tag: &'a Tag) -> Self {
        Self::from_valid_value(tag.as_ref())
    }
}

/// Parses comma- or space-separated tags.
///
/// Returns valid tags and an optional message describing invalid entries.
#[cfg(feature = "alloc")]
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
    fn borrowed_tags_support_split_and_formatted_values() {
        let split = TagRef::new("env", "staging:east").unwrap();
        let formatted = TagRef::from_value("env:staging:east").unwrap();
        let unkeyed = TagRef::from_value("staging").unwrap();

        assert_eq!(split.to_string(), "env:staging:east");
        assert_eq!(formatted.parts(), (Some("env"), "staging:east"));
        assert_eq!(unkeyed.parts(), (None, "staging"));
    }

    #[test]
    fn borrowed_tags_reject_invalid_boundaries() {
        assert!(matches!(TagRef::from_value(""), Err(TagError::Empty)));
        assert!(matches!(
            TagRef::from_value(":value"),
            Err(TagError::BeginsWithColon)
        ));
        assert!(matches!(
            TagRef::from_value("value:"),
            Err(TagError::EndsWithColon)
        ));
        assert!(matches!(
            TagRef::new("", "value"),
            Err(TagError::BeginsWithColon)
        ));
        assert!(matches!(
            TagRef::new("key", ""),
            Err(TagError::EndsWithColon)
        ));
    }

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
