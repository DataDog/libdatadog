// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

#[derive(Clone, Eq, PartialEq)]
pub struct Tag {
    key: Cow<'static, str>,
    value: Cow<'static, str>,
}

impl Debug for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tag")
            .field("key", &self.key)
            .field("value", &self.value)
            .finish()
    }
}

// Any type which implements Display automatically has to_string.
impl Display for Tag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // A tag isn't supposed to end with a colon, so if there isn't a value
        // then don't follow the tag with a colon.
        if self.value.is_empty() {
            write!(f, "{}", self.key)
        } else {
            write!(f, "{}:{}", self.key, self.value)
        }
    }
}

impl Tag {
    pub fn new<IntoCow: Into<Cow<'static, str>>>(
        key: IntoCow,
        value: IntoCow,
    ) -> Result<Self, Cow<'static, str>> {
        let key = key.into();
        let value = value.into();
        if key.is_empty() {
            return Err("tag key was empty".into());
        }

        let first_valid_char = key
            .chars()
            .find(|char| *char != std::char::REPLACEMENT_CHARACTER && !char.is_whitespace());

        if first_valid_char.is_none() {
            return Err("tag contained only whitespace or invalid unicode characters".into());
        }

        Ok(Self { key, value })
    }

    pub fn key(&self) -> &Cow<str> {
        &self.key
    }
    pub fn value(&self) -> &Cow<str> {
        &self.value
    }

    pub fn into_owned(mut self) -> Self {
        self.key = self.key.to_owned();
        self.value = self.value.to_owned();
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::Tag;

    #[test]
    fn test_empty_key() {
        let _ = Tag::new("", "woof").expect_err("empty key is not allowed");
    }

    #[test]
    fn test_empty_value() {
        let tag = Tag::new("key1", "").expect("empty value is okay");
        assert_eq!("key1", tag.to_string()); // notice no trailing colon!
    }

    #[test]
    fn test_bad_utf8() {
        // 0b1111_0xxx is the start of a 4-byte sequence, but there aren't any
        // more chars, so it  will get converted into the utf8 replacement
        // character. This results in a string with a space (32) and a
        // replacement char, so it should be an error (no valid chars).
        let bytes = &[32, 0b1111_0111];
        let key = String::from_utf8_lossy(bytes);
        let _ = Tag::new(key, "value".into()).expect_err("invalid tag is rejected");
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
            (":key-starts-with-colon".to_string(), "value".to_owned()),
            ("key".to_string(), "value-ends-with-colon:".to_owned()),
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
}
