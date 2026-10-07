// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Datadog tag construction and validation.
//!
//! # Validation policy
//!
//! The Datadog documentation defines more tag rules than this module enforces:
//! <https://docs.datadoghq.com/getting_started/tagging/#define-tags>. Enforcing rules that tracing
//! and profiling do not apply consistently would produce a worse user experience, so runtime
//! validation currently rejects only empty tags and likely colon-related mistakes. Compile-time
//! validation by the [`tag!`] macro is intentionally stricter.

#[cfg(feature = "alloc")]
use alloc::{
    borrow::{Cow, ToOwned},
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
#[cfg(feature = "alloc")]
use core::fmt::Debug;
use core::fmt::{Display, Formatter};
#[cfg(feature = "alloc")]
use serde::Deserialize;
use serde::{Serialize, Serializer};

#[cfg(feature = "alloc")]
pub use static_assertions::{const_assert, const_assert_ne};

/// Describes some reasons why a tag is invalid.
#[allow(missing_docs, reason = "variant names are self-documenting")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive] // so we can add more cases without breaking semver
pub enum TagValidationError {
    Empty,
    BeginsWithColon,
    EndsWithColon,
}

impl Display for TagValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Empty => "tag is empty",
            Self::BeginsWithColon => "tag begins with a colon",
            Self::EndsWithColon => "tag ends with a colon",
        })
    }
}

impl core::error::Error for TagValidationError {}

/// A tag rejected while validating a tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTag<'a> {
    /// The invalid serialized tag.
    pub value: &'a str,
    /// Why the tag is invalid.
    pub error: TagValidationError,
}

impl Display for InvalidTag<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self.error {
            TagValidationError::Empty => f.write_str("tag is empty"),
            TagValidationError::BeginsWithColon => {
                write!(f, "tag '{}' begins with a colon", self.value)
            }
            TagValidationError::EndsWithColon => {
                write!(f, "tag '{}' ends with a colon", self.value)
            }
        }
    }
}

impl core::error::Error for InvalidTag<'_> {}

/// Validates a tag key and value pair.
#[inline]
fn validate_pair(key: &str, value: &str) -> Result<(), TagValidationError> {
    if key.is_empty() || key.starts_with(':') {
        Err(TagValidationError::BeginsWithColon)
    } else if value.is_empty() || value.ends_with(':') {
        Err(TagValidationError::EndsWithColon)
    } else {
        Ok(())
    }
}

/// Validates a tag that has already been serialized.
#[inline]
fn validate_serialized(value: &str) -> Result<(), TagValidationError> {
    if value.is_empty() {
        Err(TagValidationError::Empty)
    } else if value.starts_with(':') {
        Err(TagValidationError::BeginsWithColon)
    } else if value.ends_with(':') {
        Err(TagValidationError::EndsWithColon)
    } else {
        Ok(())
    }
}

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
    pub fn new(key: &'a str, value: &'a str) -> Result<Self, TagValidationError> {
        validate_pair(key, value)?;
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
    pub fn from_value(value: &'a str) -> Result<Self, TagValidationError> {
        validate_serialized(value)?;
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

impl Display for TagRef<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if let Some(key) = self.key {
            f.write_str(key)?;
            f.write_str(":")?;
        }
        f.write_str(self.value)
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

#[cfg(feature = "alloc")]
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag {
    /// Many tags are made from literal strings, such as:
    ///  - "language:native"
    ///  - "src_library:libdatadog"
    ///  - "type:timeout"
    ///
    /// So being able to save allocations is nice.
    value: Cow<'static, str>,
}

#[cfg(feature = "alloc")]
impl Tag {
    /// Used by the `tag!` macro. Not meant to be used directly, please use
    /// the macro instead.
    /// # Safety
    /// Do not use directly, use through the `tag!` macro which enforces the
    /// safety invariants at compile time.
    pub const unsafe fn from_static_unchecked(value: &'static str) -> Self {
        Self {
            value: Cow::Borrowed(value),
        }
    }
}

/// Creates a tag from a key and value known at compile-time, and fails to
/// compile if it's known to be invalid (it may still emit an invalid tag, not
/// all tag validation is currently done client-side). If the key or value
/// aren't known at compile-time, then use [Tag::new].
// todo: what's a good way to keep these in-sync with Tag::from_value?
// This can be a little more strict because it's compile-time evaluated.
// https://docs.datadoghq.com/getting_started/tagging/#define-tags
#[macro_export]
#[cfg(feature = "alloc")]
macro_rules! tag {
    ($key:expr, $val:expr) => {{
        // Keys come in "value" or "key:value" format. This pattern is always
        // the key:value format, which means the value should not be empty.
        // todo: the implementation here differs subtly from Tag::from_value,
        //       which checks that the whole thing doesn't end with a colon.
        $crate::tag::const_assert!(!$val.is_empty());

        const COMBINED: &'static str = $crate::const_format::concatcp!($key, ":", $val);

        // Tags must start with a letter. This is more restrictive than is
        // required (could be a unicode alphabetic char) and can be lifted
        // if it's causing problems.
        $crate::tag::const_assert!(COMBINED.as_bytes()[0].is_ascii_alphabetic());

        // Tags can be up to 200 characters long and support Unicode letters
        // (which includes most character sets, including languages such as
        // Japanese).
        // Presently, engineers interpretted this to be 200 bytes, not unicode
        // characters. However, if the 200th character is unicode, it's
        // allowed to spill over due to a historical bug. For now, we'll
        // ignore this and hard-code 200 bytes.
        $crate::tag::const_assert!(COMBINED.as_bytes().len() <= 200);

        #[allow(unused_unsafe)]
        let tag = unsafe { $crate::tag::Tag::from_static_unchecked(COMBINED) };
        tag
    }};
}

#[cfg(feature = "alloc")]
impl Debug for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tag").field("value", &self.value).finish()
    }
}

#[cfg(feature = "alloc")]
impl AsRef<str> for Tag {
    fn as_ref(&self) -> &str {
        self.value.as_ref()
    }
}

// Any type which implements Display automatically has to_string.
#[cfg(feature = "alloc")]
impl Display for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.value)
    }
}

#[cfg(feature = "alloc")]
impl Tag {
    /// Validates a tag key and value pair.
    #[inline]
    pub fn validate(key: &str, value: &str) -> Result<(), TagValidationError> {
        validate_pair(key, value)
    }

    /// Validates a tag that has already been serialized.
    #[inline]
    pub fn validate_value(value: &str) -> Result<(), TagValidationError> {
        validate_serialized(value)
    }

    /// Validates a tag.
    fn from_value<'a, IntoCow>(value: IntoCow) -> anyhow::Result<Self>
    where
        IntoCow: Into<Cow<'a, str>>,
    {
        let value = value.into();
        match Self::validate_value(&value) {
            Ok(()) => Ok(Self {
                value: Cow::Owned(value.into_owned()),
            }),
            Err(error) => {
                let invalid = InvalidTag {
                    value: value.as_ref(),
                    error,
                };
                anyhow::bail!("{invalid}")
            }
        }
    }

    /// Creates a tag from a key and value. It's preferred to use the `tag!`
    /// macro when the key and value are both known at compile-time.
    pub fn new<K, V>(key: K, value: V) -> anyhow::Result<Self>
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        Tag::from_value(format!("{key}:{value}"))
    }
}

/// An allocation-free iterator over validated tags in a comma- or space-separated string.
pub struct TagParser<'a> {
    chunks: core::str::Split<'a, &'static [char]>,
}

impl<'a> TagParser<'a> {
    /// Creates an iterator over the tags in `input`.
    pub fn new(input: &'a str) -> Self {
        const SEPARATORS: &[char] = &[',', ' '];
        Self {
            chunks: input.split(SEPARATORS),
        }
    }
}

impl<'a> Iterator for TagParser<'a> {
    type Item = Result<&'a str, InvalidTag<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let tag = self.chunks.find(|chunk| !chunk.is_empty())?;
        Some(
            validate_serialized(tag)
                .map(|()| tag)
                .map_err(|error| InvalidTag { value: tag, error }),
        )
    }
}

#[cfg(feature = "alloc")]
impl<'a> From<&'a Tag> for TagRef<'a> {
    fn from(tag: &'a Tag) -> Self {
        TagRef::from_valid_value(tag.as_ref())
    }
}

/// Parse a string of tags typically provided by environment variables
/// The tags are expected to be either space or comma separated:
///     "key1:value1,key2:value2"
///     "key1:value1 key2:value2"
/// Tag names and values are required and may not be empty.
///
/// Returns a tuple of the correctly parsed tags and an optional error message
/// describing issues encountered during parsing.
#[cfg(feature = "alloc")]
pub fn parse_tags(str: &str) -> (Vec<Tag>, Option<String>) {
    let mut tags = vec![];
    let mut error_message = String::new();
    for result in TagParser::new(str) {
        match result {
            Ok(tag) => tags.push(Tag {
                value: Cow::Owned(tag.to_owned()),
            }),
            Err(err) => {
                if error_message.is_empty() {
                    error_message += "Errors while parsing tags: ";
                } else {
                    error_message += ", ";
                }
                error_message += &err.to_string();
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

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_component_and_serialized_validation_are_consistent(
            key in any::<String>(),
            value in any::<String>(),
        ) {
            let serialized = format!("{key}:{value}");

            prop_assert_eq!(
                Tag::validate(&key, &value),
                Tag::validate_value(&serialized)
            );
        }

        #[test]
        fn prop_constructor_and_validation_are_consistent(
            key in any::<String>(),
            value in any::<String>(),
        ) {
            let serialized = format!("{key}:{value}");
            let validation = Tag::validate(&key, &value);
            let result = Tag::new(&key, &value);

            prop_assert_eq!(result.is_ok(), validation.is_ok());
            if let Ok(tag) = result {
                prop_assert_eq!(tag.as_ref(), serialized);
            }
        }

        #[test]
        fn prop_serialized_constructor_and_validation_are_consistent(
            value in any::<String>(),
        ) {
            prop_assert_eq!(
                Tag::from_value(value.as_str()).is_ok(),
                Tag::validate_value(&value).is_ok()
            );
        }
    }

    #[test]
    fn test_is_send() {
        // fails to compile if false
        fn is_send<T: Send>(_t: T) -> bool {
            true
        }
        assert!(is_send(tag!("src_library", "libdatadog")));
    }

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
        assert_eq!(
            TagRef::from_value("").unwrap_err(),
            TagValidationError::Empty
        );
        assert_eq!(
            TagRef::from_value(":value").unwrap_err(),
            TagValidationError::BeginsWithColon
        );
        assert_eq!(
            TagRef::from_value("value:").unwrap_err(),
            TagValidationError::EndsWithColon
        );
        assert_eq!(
            TagRef::new("", "value").unwrap_err(),
            TagValidationError::BeginsWithColon
        );
        assert_eq!(
            TagRef::new("key", "").unwrap_err(),
            TagValidationError::EndsWithColon
        );
    }

    #[test]
    fn borrowed_tag_from_owned_tag() {
        let tag = Tag::new("env", "staging:east").unwrap();
        assert_eq!(TagRef::from(&tag).parts(), (Some("env"), "staging:east"));
    }

    #[test]
    fn test_validation() {
        assert_eq!(Tag::validate("key", "value"), Ok(()));
        assert_eq!(Tag::validate("key", "value:with:colons"), Ok(()));
        assert_eq!(
            Tag::validate("", "value"),
            Err(TagValidationError::BeginsWithColon)
        );
        assert_eq!(
            Tag::validate(":key", "value"),
            Err(TagValidationError::BeginsWithColon)
        );
        assert_eq!(
            Tag::validate("key", ""),
            Err(TagValidationError::EndsWithColon)
        );
        assert_eq!(
            Tag::validate("key", "value:"),
            Err(TagValidationError::EndsWithColon)
        );

        assert_eq!(Tag::validate_value("key:value"), Ok(()));
        assert_eq!(Tag::validate_value("tag"), Ok(()));
        assert_eq!(Tag::validate_value(""), Err(TagValidationError::Empty));
        assert_eq!(
            Tag::validate_value(":value"),
            Err(TagValidationError::BeginsWithColon)
        );
        assert_eq!(
            Tag::validate_value("key:"),
            Err(TagValidationError::EndsWithColon)
        );
    }

    #[test]
    fn test_empty_key() {
        let error = Tag::new("", "woof").expect_err("empty key is not allowed");
        assert_eq!(error.to_string(), "tag ':woof' begins with a colon");
    }

    #[test]
    fn test_empty_value() {
        let error = Tag::new("key1", "").expect_err("empty value is an error");
        assert_eq!(error.to_string(), "tag 'key1:' ends with a colon");
    }

    #[test]
    fn test_bad_utf8() {
        // 0b1111_0xxx is the start of a 4-byte sequence, but there aren't any
        // more chars, so it  will get converted into the utf8 replacement
        // character. This results in a string with an "a" and a replacement
        // char, so it should be an error (no valid chars). However, we don't
        // enforce many things about tags yet client-side, so we let it slide.
        let bytes = &[b'a', 0b1111_0111];
        let key = String::from_utf8_lossy(bytes);
        let t = Tag::new(key, "value").unwrap();
        assert_eq!("a\u{FFFD}:value", t.to_string());
    }

    #[test]
    fn test_value_has_colon() {
        let result = Tag::new("env", "staging:east").expect("values can have colons");
        assert_eq!("env:staging:east", result.to_string());

        let result = tag!("env", "staging:east");
        assert_eq!("env:staging:east", result.to_string());
    }

    #[test]
    fn test_suspicious_tags() {
        // Based on tag rules, these should all fail. However, there is a risk
        // that profile tags will then differ or cause failures compared to
        // trace tags. These require cross-team, cross-language collaboration.
        let cases = [
            ("_begins_with_non-letter".to_string(), "value"),
            ("the-tag-length-is-over-200-characters".repeat(6), "value"),
        ];

        for case in cases {
            let result = Tag::new(case.0, case.1);
            // Again, these should fail, but it's not implemented yet
            assert!(result.is_ok())
        }
    }

    #[test]
    fn test_missing_colon_parsing() {
        let tag = Tag::from_value("tag").unwrap();
        assert_eq!("tag", tag.to_string());
    }

    #[test]
    fn test_leading_colon_parsing() {
        let _ = Tag::from_value(":tag").expect_err("Cannot start with a colon");
    }

    #[test]
    fn test_tailing_colon_parsing() {
        let _ = Tag::from_value("tag:").expect_err("Cannot end with a colon");
    }

    #[test]
    fn test_tag_parser() {
        let parsed =
            TagParser::new("key:value, :leading middle:colon trailing:  bare").collect::<Vec<_>>();
        assert_eq!(
            parsed,
            vec![
                Ok("key:value"),
                Err(InvalidTag {
                    value: ":leading",
                    error: TagValidationError::BeginsWithColon,
                }),
                Ok("middle:colon"),
                Err(InvalidTag {
                    value: "trailing:",
                    error: TagValidationError::EndsWithColon,
                }),
                Ok("bare"),
            ]
        );
    }

    #[test]
    fn test_tag_parser_preserves_parse_tags_errors() {
        let (tags, error) = parse_tags("valid:value,:leading,trailing:,also:valid");
        assert_eq!(
            tags,
            vec![
                Tag::new("valid", "value").unwrap(),
                Tag::new("also", "valid").unwrap(),
            ]
        );
        assert_eq!(
            error.as_deref(),
            Some(
                "Errors while parsing tags: tag ':leading' begins with a colon, tag 'trailing:' ends with a colon"
            )
        );
    }

    #[test]
    fn test_tags_parsing() {
        let cases = [
            ("", vec![]),
            (",", vec![]),
            (" , ", vec![]),
            // Testing that values can contain colons
            (
                "env:staging:east,location:nyc:ny",
                vec![
                    Tag::new("env", "staging:east").unwrap(),
                    Tag::new("location", "nyc:ny").unwrap(),
                ],
            ),
            // Testing value format (no key)
            ("value", vec![Tag::from_value("value").unwrap()]),
            (
                "state:utah,state:idaho",
                vec![
                    Tag::new("state", "utah").unwrap(),
                    Tag::new("state", "idaho").unwrap(),
                ],
            ),
            (
                "key1:value1 key2:value2 key3:value3",
                vec![
                    Tag::new("key1", "value1").unwrap(),
                    Tag::new("key2", "value2").unwrap(),
                    Tag::new("key3", "value3").unwrap(),
                ],
            ),
            (
                // Testing consecutive separators being collapsed
                "key1:value1, key2:value2 ,key3:value3 , key4:value4",
                vec![
                    Tag::new("key1", "value1").unwrap(),
                    Tag::new("key2", "value2").unwrap(),
                    Tag::new("key3", "value3").unwrap(),
                    Tag::new("key4", "value4").unwrap(),
                ],
            ),
        ];

        for case in cases {
            let expected = case.1;
            let (actual, error_message) = parse_tags(case.0);
            assert_eq!(expected, actual);
            assert!(error_message.is_none());
        }
    }
}
