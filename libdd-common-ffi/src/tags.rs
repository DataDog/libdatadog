// Copyright 2022-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "std")]

use crate::Error;
use crate::slice::{AsBytes, CharSlice};
use alloc::borrow::Cow;
use alloc::string::String;
use libdd_common::tag::{Tag, TagBuildError, TagValidationError, parse_tags};

/// Creates an empty tag vector. The caller owns the returned vector and must eventually pass it to
/// `ddog_Vec_Tag_drop`.
// allow(ffi-panic-boundary): constructing an empty Vec does not allocate or panic.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn ddog_Vec_Tag_new() -> crate::Vec<Tag> {
    crate::Vec::default()
}

/// Consumes the tag vector and frees all storage owned by it. The vector must not be used
/// afterward.
// allow(ffi-panic-boundary): dropping Vec<Tag> only releases owned string storage.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_Vec_Tag_drop(_: crate::Vec<Tag>) {}

#[repr(C)]
pub enum PushTagResult {
    Ok,
    Err(Error),
}

/// Creates a new Tag from the provided `key` and `value` by doing a utf8
/// lossy conversion, and pushes into the `vec`. The strings `key` and `value`
/// are cloned to avoid FFI lifetime issues.
///
/// On success, `vec` owns copies of the key and value. On error, the vector's elements are
/// unchanged and the call may be retried. The caller owns an error returned in `PushTagResult::Err`
/// and must clear it with `ddog_Error_drop`.
///
/// # Safety
/// The `vec` must be a valid reference.
/// The CharSlices `key` and `value` must point to at least many bytes as their
/// `.len` properties claim.
// allow(ffi-panic-boundary): safety preconditions make slices valid and every allocation is
// fallible.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_Vec_Tag_push(
    vec: &mut crate::Vec<Tag>,
    key: CharSlice,
    value: CharSlice,
) -> PushTagResult {
    let key = match try_to_utf8_lossy(key.as_bytes()) {
        Ok(key) => key,
        Err(_) => return PushTagResult::Err(Error::out_of_memory()),
    };
    let value = match try_to_utf8_lossy(value.as_bytes()) {
        Ok(value) => value,
        Err(_) => return PushTagResult::Err(Error::out_of_memory()),
    };
    let tag = match Tag::try_new(key.as_ref(), value.as_ref()) {
        Ok(tag) => {
            if vec.try_reserve(1).is_err() {
                return PushTagResult::Err(Error::out_of_memory());
            }
            tag
        }
        Err(TagBuildError::OutOfMemory) => {
            return PushTagResult::Err(Error::out_of_memory());
        }
        Err(TagBuildError::Validation(error)) => {
            return PushTagResult::Err(Error::from_display(InvalidTagPair {
                key: key.as_ref(),
                value: value.as_ref(),
                error,
            }));
        }
    };

    // try_reserve above guarantees this push does not allocate.
    vec.push(tag);
    PushTagResult::Ok
}

struct InvalidTagPair<'a> {
    key: &'a str,
    value: &'a str,
    error: TagValidationError,
}

impl core::fmt::Display for InvalidTagPair<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.error {
            TagValidationError::BeginsWithColon => {
                write!(f, "tag '{}:{}' begins with a colon", self.key, self.value)
            }
            TagValidationError::EndsWithColon => {
                write!(f, "tag '{}:{}' ends with a colon", self.key, self.value)
            }
            _ => write!(f, "invalid tag '{}:{}'", self.key, self.value),
        }
    }
}

fn try_to_utf8_lossy(bytes: &[u8]) -> Result<Cow<'_, str>, alloc::collections::TryReserveError> {
    if let Ok(valid) = core::str::from_utf8(bytes) {
        return Ok(Cow::Borrowed(valid));
    }

    let mut output = String::new();
    for chunk in bytes.utf8_chunks() {
        let replacement_len = if chunk.invalid().is_empty() {
            0
        } else {
            '\u{FFFD}'.len_utf8()
        };
        output.try_reserve(chunk.valid().len() + replacement_len)?;
        output.push_str(chunk.valid());
        if replacement_len != 0 {
            output.push('\u{FFFD}');
        }
    }
    Ok(Cow::Owned(output))
}

#[repr(C)]
pub struct ParseTagsResult {
    tags: crate::Vec<Tag>,
    error_message: Option<Box<Error>>,
}

/// # Safety
/// The `string`'s .ptr must point to a valid object at least as large as its
/// .len property.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_Vec_Tag_parse(string: CharSlice) -> ParseTagsResult {
    let string = string.to_utf8_lossy();
    let (tags, error) = parse_tags(string.as_ref());
    ParseTagsResult {
        tags: tags.into(),
        error_message: error.map(Error::from).map(Box::new),
    }
}

#[cfg(test)]
mod tests {
    use crate::tags::*;

    #[test]
    fn utf8_lossy_matches_standard_library() {
        let cases: &[&[u8]] = &[
            b"",
            b"valid",
            b"\xFF",
            b"\xC3\x28 \xFF test",
            b"\xF0\x90\x80",
            b"\xF0\x90\x80\x80",
            b"\x80\x80",
        ];

        for bytes in cases {
            assert_eq!(
                try_to_utf8_lossy(bytes).unwrap(),
                String::from_utf8_lossy(bytes)
            );
        }
        assert!(matches!(
            try_to_utf8_lossy(b"valid").unwrap(),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn empty_tag_name() {
        unsafe {
            let mut tags = ddog_Vec_Tag_new();
            let result = ddog_Vec_Tag_push(&mut tags, CharSlice::from(""), CharSlice::from("woof"));
            let PushTagResult::Err(error) = result else {
                panic!("empty tag name should fail");
            };
            assert_eq!(error.as_ref(), "tag ':woof' begins with a colon");
        }
    }

    #[test]
    fn test_lifetimes() {
        let mut tags = ddog_Vec_Tag_new();
        unsafe {
            // make a string here so it has a scoped lifetime
            let key = String::from("key1");
            {
                let value = String::from("value1");
                let result = ddog_Vec_Tag_push(
                    &mut tags,
                    CharSlice::from(key.as_str()),
                    CharSlice::from(value.as_str()),
                );

                assert!(matches!(result, PushTagResult::Ok));
            }
        }
        let tag = tags.last().unwrap();
        assert_eq!("key1:value1", tag.to_string())
    }

    #[test]
    fn test_get() {
        unsafe {
            let mut tags = ddog_Vec_Tag_new();
            let result =
                ddog_Vec_Tag_push(&mut tags, CharSlice::from("sound"), CharSlice::from("woof"));
            assert!(matches!(result, PushTagResult::Ok));
            assert_eq!(1, tags.len());
            assert_eq!("sound:woof", tags.first().unwrap().to_string());
        }
    }

    #[test]
    fn test_parse() {
        let dd_tags = "env:staging:east, tags:, env_staging:east"; // contains an error

        // SAFETY: CharSlices from Rust strings are safe.
        let result = unsafe { ddog_Vec_Tag_parse(CharSlice::from(dd_tags)) };
        assert_eq!(2, result.tags.len());
        assert_eq!("env:staging:east", result.tags.first().unwrap().to_string());
        assert_eq!("env_staging:east", result.tags.get(1).unwrap().to_string());

        // 'tags:' cannot end in a semi-colon, so expect an error.
        assert!(result.error_message.is_some());
        let error = *result.error_message.unwrap();
        let error_message = error.as_ref();
        assert!(!error_message.is_empty());

        let expected_error_message = "Errors while parsing tags: tag 'tags:' ends with a colon";
        assert_eq!(expected_error_message, error_message)
    }
}
