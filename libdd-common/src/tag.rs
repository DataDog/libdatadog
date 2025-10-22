// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

pub use static_assertions::{const_assert, const_assert_ne};

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

impl Debug for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tag").field("value", &self.value).finish()
    }
}

impl AsRef<str> for Tag {
    fn as_ref(&self) -> &str {
        self.value.as_ref()
    }
}

// Any type which implements Display automatically has to_string.
impl Display for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl Tag {
    /// Validates a tag.
    fn from_value<'a, IntoCow>(chunk: IntoCow) -> anyhow::Result<Self>
    where
        IntoCow: Into<Cow<'a, str>>,
    {
        let chunk = chunk.into();

        /* The docs have various rules, which we are choosing not to enforce:
         * https://docs.datadoghq.com/getting_started/tagging/#defining-tags
         * The reason is that if tracing and profiling disagree on what valid
         * tags are, then the user experience is degraded.
         * So... we mostly just pass it along and handle it in the backend.
         * However, we do enforce some rules around the colon, because they
         * are likely to be errors (such as passed in empty string).
         */

        anyhow::ensure!(!chunk.is_empty(), "tag is empty");

        let mut chars = chunk.chars();
        anyhow::ensure!(
            chars.next() != Some(':'),
            "tag '{chunk}' begins with a colon"
        );
        anyhow::ensure!(chars.last() != Some(':'), "tag '{chunk}' ends with a colon");

        let value = Cow::Owned(chunk.into_owned());
        Ok(Tag { value })
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

/// Parse a string of tags typically provided by environment variables
/// The tags are expected to be either space or comma separated:
///     "key1:value1,key2:value2"
///     "key1:value1 key2:value2"
/// Tag names and values are required and may not be empty.
///
/// Returns a tuple of the correctly parsed tags and an optional error message
/// describing issues encountered during parsing.
pub fn parse_tags(str: &str) -> (Vec<Tag>, Option<String>) {
    let chunks = str
        .split(&[',', ' '][..])
        .filter(|str| !str.is_empty())
        .map(Tag::from_value);

    let mut tags = vec![];
    let mut error_message = String::new();
    for result in chunks {
        match result {
            Ok(tag) => tags.push(tag),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_send() {
        // fails to compile if false
        fn is_send<T: Send>(_t: T) -> bool {
            true
        }
        assert!(is_send(tag!("src_library", "libdatadog")));
    }

    #[test]
    fn test_empty_key() {
        let _ = Tag::new("", "woof").expect_err("empty key is not allowed");
    }

    #[test]
    fn test_empty_value() {
        let _ = Tag::new("key1", "").expect_err("empty value is an error");
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
