// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

#[derive(Clone, Eq, PartialEq)]
pub struct Tag {
    value: Cow<'static, str>,
}

impl Debug for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tag").field("value", &self.value).finish()
    }
}

// Any type which implements Display automatically has to_string.
impl Display for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl Tag {
    /// It's recommended to use Tag::new when possible, as tags that are in
    /// the <KEY>:<VALUE> format are preferred.
    pub fn from_value<'a, IntoCow: Into<Cow<'a, str>>>(
        chunk: IntoCow,
    ) -> Result<Self, Cow<'static, str>> {
        let chunk = chunk.into();

        /* The docs have various rules, which we are choosing not to enforce:
         * https://docs.datadoghq.com/getting_started/tagging/#defining-tags
         * The reason is that if tracing and profiling disagree on what valid
         * tags are, then the user experience is degraded.
         * So... we mostly just pass it along and handle it in the backend.
         * However, we do enforce some rules around the colon, because they
         * are likely to be errors (such as passed in empty string).
         */

        if chunk.is_empty() {
            return Err("tag is empty".into());
        }

        let mut chars = chunk.chars();
        if chars.next() == Some(':') {
            return Err(format!("tag '{}' begins with a colon", chunk).into());
        }
        if chars.last() == Some(':') {
            return Err(format!("tag '{}' ends with a colon", chunk).into());
        }

        Ok(Tag {
            value: chunk.into_owned().into(),
        })
    }

    pub fn new<S: AsRef<str>>(key: S, value: S) -> Result<Self, Cow<'static, str>> {
        let key = key.as_ref();
        let value = value.as_ref();

        Tag::from_value(format!("{}:{}", key, value))
    }

    pub fn into_owned(mut self) -> Self {
        self.value = self.value.to_owned();
        self
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
                error_message += err.as_ref();
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
    use crate::{parse_tags, Tag};

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
        // character. This results in a string with a space (32) and a
        // replacement char, so it should be an error (no valid chars).
        // However, we don't enforce many things about tags yet, so we let it
        // slide.
        let bytes = &[32, 0b1111_0111];
        let key = String::from_utf8_lossy(bytes);
        let t = Tag::new(key.as_ref(), "value").unwrap();
        assert_eq!(" \u{FFFD}:value", t.to_string());
    }

    #[test]
    fn test_value_has_colon() {
        let result = Tag::new("env", "staging:east").expect("values can have colons");
        assert_eq!("env:staging:east", result.to_string());
    }

    #[test]
    fn test_suspicious_tags() {
        // Based on tag rules, these should all fail. However, there is a risk
        // that profile tags will then differ or cause failures compared to
        // trace tags. These require cross-team, cross-language collaboration.
        let cases = [
            (" begins with non-letter".to_string(), "value".to_owned()),
            (
                "the-tag-length-is-over-200-characters".repeat(6),
                "value".to_owned(),
            ),
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
