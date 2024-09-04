// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Bytes;
#[cfg(all(feature = "bytes_string", feature = "serde"))]
use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;
use std::str::Utf8Error;

#[cfg(feature = "bytes_string")]
pub struct BufferWrapper {
    buffer: Bytes,
}

#[cfg(feature = "bytes_string")]
impl BufferWrapper {
    /// Creates a new `BufferWrapper` from a `tinybytes::Bytes` instance.
    ///
    /// # Arguments
    ///
    /// * `buffer` - A `tinybytes::Bytes` instance to be wrapped.
    ///
    /// # Returns
    ///
    /// A new `BufferWrapper` instance containing the provided buffer.
    pub fn new(buffer: Bytes) -> Self {
        BufferWrapper { buffer }
    }

    /// Creates a `BytesString` from a slice of bytes within the wrapped buffer.
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
    pub fn create_bytes_string(&self, slice: &[u8]) -> Result<BytesString, std::str::Utf8Error> {
        BytesString::from_bytes(self.buffer.slice_ref(slice).expect("Invalid slice"))
    }

    /// Creates a `BytesString` from a slice of bytes within the wrapped buffer without validating
    /// the bytes.
    ///
    /// This function does not perform any validation on the provided bytes, and assumes that the
    /// bytes are valid UTF-8. If the bytes are not valid UTF-8, the behavior is undefined.
    ///
    /// # Arguments
    ///
    /// * `slice` - A byte slice that will be converted into a `BytesString`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it assumes the bytes are valid UTF-8. If the bytes are not
    /// valid UTF-8, the behavior is undefined.
    pub unsafe fn create_bytes_string_unchecked(&self, slice: &[u8]) -> BytesString {
        BytesString::from_bytes_unchecked(self.buffer.slice_ref(slice).expect("Invalid slice"))
    }
}

#[cfg(feature = "bytes_string")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BytesString {
    bytes: Bytes,
}

#[cfg(all(feature = "bytes_string", feature = "serde"))]
impl Serialize for BytesString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // This should be safe because we have already validated that the bytes are valid UTF-8 when
        // creating the BytesString.
        unsafe { serializer.serialize_str(self.as_str_unchecked()) }
    }
}

#[cfg(feature = "bytes_string")]
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
    pub fn from_slice(slice: &[u8]) -> Result<BytesString, std::str::Utf8Error> {
        std::str::from_utf8(slice)?;
        Ok(BytesString {
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
    pub fn from_bytes(bytes: Bytes) -> Result<BytesString, std::str::Utf8Error> {
        std::str::from_utf8(&bytes)?;
        Ok(BytesString { bytes })
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
    pub fn from_bytes_unchecked(bytes: Bytes) -> BytesString {
        BytesString { bytes }
    }

    /// Returns the string slice representation of the `BytesString`. The slice is checked to be
    /// valid UTF-8. If you use `from_bytes` or `from_slice` this check was already performed and
    /// you may want to use `as_str_unchecked` instead.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn as_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Returns the string slice representation of the `BytesString` without validating the bytes.
    /// Typically, you should use `from_slice` or `from_bytes` when creating a BytesString to
    /// ensure the bytes are valid UTF-8 (if the bytes haven't already been validated by other
    /// means) so further validation may be unnecessary.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it assumes the bytes are valid UTF-8. If the bytes are not
    /// valid UTF-8, the behavior is undefined.
    pub unsafe fn as_str_unchecked(&self) -> &str {
        // SAFETY: This is unsafe and assumes the bytes are valid UTF-8.
        unsafe { std::str::from_utf8_unchecked(&self.bytes) }
    }
}

#[cfg(feature = "bytes_string")]
impl Default for BytesString {
    fn default() -> Self {
        BytesString {
            bytes: Bytes::empty(),
        }
    }
}

#[cfg(feature = "bytes_string")]
impl Borrow<str> for BytesString {
    fn borrow(&self) -> &str {
        // This is safe because we have already validated that the bytes are valid UTF-8 when
        // creating the BytesString.
        unsafe { self.as_str_unchecked() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_from_slice() {
        let slice = b"hello";
        let bytes_string = BytesString::from_slice(slice).unwrap();
        assert_eq!(bytes_string.as_str().unwrap(), "hello");
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
        assert_eq!(bytes_string.as_str().unwrap(), "world");
    }

    #[test]
    fn test_from_bytes_invalid_utf8() {
        let invalid_utf8_bytes = Bytes::copy_from_slice(&[0, 159, 146, 150]);
        let result = BytesString::from_bytes(invalid_utf8_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes_unchecked() {
        let bytes = Bytes::copy_from_slice(b"unchecked");
        let bytes_string = BytesString::from_bytes_unchecked(bytes);
        assert_eq!(bytes_string.as_str().unwrap(), "unchecked");
    }

    #[test]
    fn test_as_str() {
        let bytes_string = BytesString::from_slice(b"test").unwrap();
        assert_eq!(bytes_string.as_str().unwrap(), "test");
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
        assert_eq!(bytes_string.as_str().unwrap(), "");
    }

    #[test]
    fn test_borrow() {
        let bytes_string = BytesString::from_slice(b"borrow").unwrap();
        let borrowed: &str = bytes_string.borrow();
        assert_eq!(borrowed, "borrow");
    }
}
