// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Bytes;
#[cfg(feature = "serde")]
use serde::ser::{Serialize, Serializer};
use std::{borrow::Borrow, hash, str::Utf8Error};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BytesString {
    bytes: Bytes,
}

#[cfg(feature = "serde")]
impl Serialize for BytesString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl BytesString {
    /// Creates a `BytesString` from a slice of bytes.
    ///
    /// This function validates that the provided slice is valid UTF-8. If the slice is not valid
    /// UTF-8, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `slice` - A byte slice that will be converted into a `BytesString`.
    ///
    /// # Returns
    ///
    /// A `Result` containing the `BytesString` if the slice is valid UTF-8, or a `Utf8Error` if
    /// the slice is not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn from_slice(slice: &[u8]) -> Result<Self, Utf8Error> {
        std::str::from_utf8(slice)?;
        Ok(Self {
            bytes: Bytes::copy_from_slice(slice),
        })
    }

    /// Creates a `BytesString` from a `tinybytes::Bytes` instance.
    ///
    /// This function validates that the provided `Bytes` instance contains valid UTF-8 data. If the
    /// data is not valid UTF-8, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `bytes` - A `tinybytes::Bytes` instance that will be converted into a `BytesString`.
    ///
    /// # Returns
    ///
    /// A `Result` containing the `BytesString` if the bytes are valid UTF-8, or a `Utf8Error` if
    /// the bytes are not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn from_bytes(bytes: Bytes) -> Result<Self, Utf8Error> {
        std::str::from_utf8(&bytes)?;
        Ok(Self { bytes })
    }

    /// Creates a `BytesString` from a string slice within the given buffer.
    ///
    /// # Arguments
    ///
    /// * `bytes` - A `tinybytes::Bytes` instance that will be converted into a `BytesString`.
    /// * `slice` - The string slice pointing into the given bytes that will form the `BytesString`.
    pub fn from_bytes_slice(bytes: &Bytes, slice: &str) -> Self {
        // SAFETY: This is safe as a str slice is definitely a valid UTF-8 slice.
        unsafe {
            Self::from_bytes_unchecked(bytes.slice_ref(slice.as_bytes()).expect("Invalid slice"))
        }
    }

    /// Creates a `BytesString` from a `tinybytes::Bytes` instance without validating the bytes.
    ///
    /// This function does not perform any validation on the provided bytes, and assumes that the
    /// bytes are valid UTF-8. If the bytes are not valid UTF-8, the behavior is undefined.
    ///
    /// # Arguments
    ///
    /// * `bytes` - A `tinybytes::Bytes` instance that will be converted into a `BytesString`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it assumes the bytes are valid UTF-8. If the bytes are not
    /// valid UTF-8, the behavior is undefined.
    pub unsafe fn from_bytes_unchecked(bytes: Bytes) -> Self {
        Self { bytes }
    }

    /// Returns the string slice representation of the `BytesString` (without validating the bytes).
    /// Typically, you should use `from_slice` or `from_bytes` when creating a BytesString to
    /// ensure the bytes are valid UTF-8 (if the bytes haven't already been validated by other
    /// means) so further validation may be unnecessary.
    pub fn as_str(&self) -> &str {
        // SAFETY: We assume all BytesStrings are valid UTF-8.
        unsafe { std::str::from_utf8_unchecked(&self.bytes) }
    }
}

impl Default for BytesString {
    fn default() -> Self {
        Self {
            bytes: Bytes::empty(),
        }
    }
}

impl Borrow<str> for BytesString {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for BytesString {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<String> for BytesString {
    fn from(value: String) -> Self {
        // SAFETY: This is safe as a String is always a valid UTF-8 slice.
        unsafe { Self::from_bytes_unchecked(Bytes::from_underlying(value)) }
    }
}

impl From<&'static str> for BytesString {
    fn from(value: &'static str) -> Self {
        // SAFETY: This is safe as a str is always a valid UTF-8 slice.
        unsafe { Self::from_bytes_unchecked(Bytes::from_static(value.as_bytes())) }
    }
}

// We can't derive Hash from Bytes as [u8] and str do not provide the same hash
impl hash::Hash for BytesString {
    #[inline]
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{DefaultHasher, Hash, Hasher};

    #[test]
    fn test_from_slice() {
        let slice = b"hello";
        let bytes_string = BytesString::from_slice(slice).unwrap();
        assert_eq!(bytes_string.as_str(), "hello");
    }

    #[test]
    fn test_from_slice_invalid_utf8() {
        let invalid_utf8_slice = &[0, 159, 146, 150];
        let result = BytesString::from_slice(invalid_utf8_slice);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes() {
        let bytes = Bytes::copy_from_slice(b"world");
        let bytes_string = BytesString::from_bytes(bytes).unwrap();
        assert_eq!(bytes_string.as_str(), "world");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_from_bytes_invalid_utf8() {
        let invalid_utf8_bytes = Bytes::copy_from_slice(&[0, 159, 146, 150]);
        let result = BytesString::from_bytes(invalid_utf8_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes_unchecked() {
        let bytes = Bytes::copy_from_slice(b"unchecked");
        let bytes_string = unsafe { BytesString::from_bytes_unchecked(bytes) };
        assert_eq!(bytes_string.as_str(), "unchecked");
    }

    #[test]
    fn test_as_str() {
        let bytes_string = BytesString::from_slice(b"test").unwrap();
        assert_eq!(bytes_string.as_str(), "test");
    }

    #[test]
    fn test_serialize() {
        let bytes_string = BytesString::from_slice(b"serialize");
        let serialized = serde_json::to_string(&bytes_string.unwrap()).unwrap();
        assert_eq!(serialized, "\"serialize\"");
    }

    #[test]
    fn test_default() {
        let bytes_string: BytesString = Default::default();
        assert_eq!(bytes_string.as_str(), "");
    }

    #[test]
    fn test_borrow() {
        let bytes_string = BytesString::from_slice(b"borrow").unwrap();
        let borrowed: &str = bytes_string.borrow();
        assert_eq!(borrowed, "borrow");
    }

    #[test]
    fn from_string() {
        let string = String::from("hello");
        let bytes_string = BytesString::from(string);
        assert_eq!(bytes_string.as_str(), "hello")
    }

    #[test]
    fn from_static_str() {
        let static_str = "hello";
        let bytes_string = BytesString::from(static_str);
        assert_eq!(bytes_string.as_str(), "hello")
    }

    fn calculate_hash<T: Hash>(t: &T) -> u64 {
        let mut s = DefaultHasher::new();
        t.hash(&mut s);
        s.finish()
    }

    #[test]
    fn hash() {
        let bytes_string = BytesString::from_slice(b"test hash").unwrap();
        assert_eq!(calculate_hash(&bytes_string), calculate_hash(&"test hash"));
    }
}
