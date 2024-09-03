use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;
use std::str::Utf8Error;
use tinybytes;

pub struct BufferWrapper {
    buffer: tinybytes::Bytes,
}

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
    pub fn new(buffer: tinybytes::Bytes) -> Self {
        BufferWrapper { buffer }
    }

    /// Creates a `NoAllocString` from a slice of bytes within the wrapped buffer.
    ///
    /// This function validates that the provided slice is valid UTF-8. If the slice is not valid
    /// UTF-8, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `slice` - A byte slice that will be converted into a `NoAllocString`.
    ///
    /// # Returns
    ///
    /// A `Result` containing the `NoAllocString` if the slice is valid UTF-8, or a `Utf8Error` if
    /// the slice is not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn create_no_alloc_string(
        &self,
        slice: &[u8],
    ) -> Result<NoAllocString, std::str::Utf8Error> {
        NoAllocString::from_bytes(self.buffer.slice_ref(slice).expect("Invalid slice"))
    }

    /// Creates a `NoAllocString` from a slice of bytes within the wrapped buffer without validating
    /// the bytes.
    ///
    /// This function does not perform any validation on the provided bytes, and assumes that the
    /// bytes are valid UTF-8. If the bytes are not valid UTF-8, the behavior is undefined.
    ///
    /// # Arguments
    ///
    /// * `slice` - A byte slice that will be converted into a `NoAllocString`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it assumes the bytes are valid UTF-8. If the bytes are not
    /// valid UTF-8, the behavior is undefined.
    pub unsafe fn create_no_alloc_string_unchecked(&self, slice: &[u8]) -> NoAllocString {
        NoAllocString::from_bytes_unchecked(self.buffer.slice_ref(slice).expect("Invalid slice"))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NoAllocString {
    bytes: tinybytes::Bytes,
}

impl Serialize for NoAllocString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // This should be safe because we have already validated that the bytes are valid UTF-8 when
        // creating the NoAllocString.
        unsafe { serializer.serialize_str(self.as_str_unchecked()) }
    }
}

impl NoAllocString {
    /// Creates a `NoAllocString` from a slice of bytes.
    ///
    /// This function validates that the provided slice is valid UTF-8. If the slice is not valid
    /// UTF-8, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `slice` - A byte slice that will be converted into a `NoAllocString`.
    ///
    /// # Returns
    ///
    /// A `Result` containing the `NoAllocString` if the slice is valid UTF-8, or a `Utf8Error` if
    /// the slice is not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn from_slice(slice: &[u8]) -> Result<NoAllocString, std::str::Utf8Error> {
        std::str::from_utf8(slice)?;
        Ok(NoAllocString {
            bytes: tinybytes::Bytes::copy_from_slice(slice),
        })
    }

    /// Creates a `NoAllocString` from a `tinybytes::Bytes` instance.
    ///
    /// This function validates that the provided `Bytes` instance contains valid UTF-8 data. If the
    /// data is not valid UTF-8, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `bytes` - A `tinybytes::Bytes` instance that will be converted into a `NoAllocString`.
    ///
    /// # Returns
    ///
    /// A `Result` containing the `NoAllocString` if the bytes are valid UTF-8, or a `Utf8Error` if
    /// the bytes are not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn from_bytes(bytes: tinybytes::Bytes) -> Result<NoAllocString, std::str::Utf8Error> {
        std::str::from_utf8(&bytes)?;
        Ok(NoAllocString { bytes })
    }

    /// Creates a `NoAllocString` from a `tinybytes::Bytes` instance without validating the bytes.
    ///
    /// This function does not perform any validation on the provided bytes, and assumes that the
    /// bytes are valid UTF-8. If the bytes are not valid UTF-8, the behavior is undefined.
    ///
    /// # Arguments
    ///
    /// * `bytes` - A `tinybytes::Bytes` instance that will be converted into a `NoAllocString`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it assumes the bytes are valid UTF-8. If the bytes are not
    /// valid UTF-8, the behavior is undefined.
    pub fn from_bytes_unchecked(bytes: tinybytes::Bytes) -> NoAllocString {
        NoAllocString { bytes }
    }

    /// Returns the string slice representation of the `NoAllocString`. The slice is checked to be
    /// valid UTF-8. If you use `from_bytes` or `from_slice` this check was already performed and
    /// you may want to use `as_str_unchecked` instead.
    ///
    /// # Errors
    ///
    /// Returns a `Utf8Error` if the bytes are not valid UTF-8.
    pub fn as_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Returns the string slice representation of the `NoAllocString` without validating the bytes.
    /// Typically, you should use `from_slice` or `from_bytes` when creating a NoAllocString to
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

impl Default for NoAllocString {
    fn default() -> Self {
        NoAllocString {
            bytes: tinybytes::Bytes::empty(),
        }
    }
}

impl Borrow<str> for NoAllocString {
    fn borrow(&self) -> &str {
        // This is safe because we have already validated that the bytes are valid UTF-8 when
        // creating the NoAllocString.
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
        let no_alloc_string = NoAllocString::from_slice(slice).unwrap();
        assert_eq!(no_alloc_string.as_str().unwrap(), "hello");
    }

    #[test]
    fn test_from_slice_invalid_utf8() {
        let invalid_utf8_slice = &[0, 159, 146, 150];
        let result = NoAllocString::from_slice(invalid_utf8_slice);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes() {
        let bytes = tinybytes::Bytes::copy_from_slice(b"world");
        let no_alloc_string = NoAllocString::from_bytes(bytes).unwrap();
        assert_eq!(no_alloc_string.as_str().unwrap(), "world");
    }

    #[test]
    fn test_from_bytes_invalid_utf8() {
        let invalid_utf8_bytes = tinybytes::Bytes::copy_from_slice(&[0, 159, 146, 150]);
        let result = NoAllocString::from_bytes(invalid_utf8_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes_unchecked() {
        let bytes = tinybytes::Bytes::copy_from_slice(b"unchecked");
        let no_alloc_string = NoAllocString::from_bytes_unchecked(bytes);
        assert_eq!(no_alloc_string.as_str().unwrap(), "unchecked");
    }

    #[test]
    fn test_as_str() {
        let no_alloc_string = NoAllocString::from_slice(b"test").unwrap();
        assert_eq!(no_alloc_string.as_str().unwrap(), "test");
    }

    #[test]
    fn test_serialize() {
        let no_alloc_string = NoAllocString::from_slice(b"serialize");
        let serialized = serde_json::to_string(&no_alloc_string.unwrap()).unwrap();
        assert_eq!(serialized, "\"serialize\"");
    }

    #[test]
    fn test_default() {
        let no_alloc_string: NoAllocString = Default::default();
        assert_eq!(no_alloc_string.as_str().unwrap(), "");
    }

    #[test]
    fn test_borrow() {
        let no_alloc_string = NoAllocString::from_slice(b"borrow").unwrap();
        let borrowed: &str = no_alloc_string.borrow();
        assert_eq!(borrowed, "borrow");
    }
}
