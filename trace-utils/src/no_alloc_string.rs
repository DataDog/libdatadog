use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;
use tinybytes;

pub struct BufferWrapper {
    buffer: tinybytes::Bytes,
}

impl BufferWrapper {
    pub fn new(buffer: tinybytes::Bytes) -> Self {
        BufferWrapper { buffer }
    }

    /// Creates a NoAllocString from a slice of tinybytes::Bytes.
    pub fn create_no_alloc_string(
        &self,
        slice: &[u8],
    ) -> Result<NoAllocString, std::str::Utf8Error> {
        NoAllocString::from_bytes(self.buffer.slice_ref(slice).expect("Invalid slice"))
    }

    /// Creates a NoAllocString from a slice of tinybytes::Bytes without validating the bytes are
    /// UTF-8.
    pub fn create_no_alloc_string_unchecked(&self, slice: &[u8]) -> NoAllocString {
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
        serializer.serialize_str(self.as_str())
    }
}

impl NoAllocString {
    // Creates a NoAllocString from a full slice (copies the data, so technically not no-alloc)
    pub fn from_slice(slice: &[u8]) -> Result<NoAllocString, std::str::Utf8Error> {
        std::str::from_utf8(slice)?;
        Ok(NoAllocString {
            bytes: tinybytes::Bytes::copy_from_slice(slice),
        })
    }
    // Creates a NoAllocString from a Bytes instance (does not copy the data)
    pub fn from_bytes(bytes: tinybytes::Bytes) -> Result<NoAllocString, std::str::Utf8Error> {
        std::str::from_utf8(&bytes)?;
        Ok(NoAllocString { bytes })
    }

    /// Creates a NoAllocString from a Bytes instance without validating the bytes are UTF-8.
    pub fn from_bytes_unchecked(bytes: tinybytes::Bytes) -> NoAllocString {
        NoAllocString { bytes }
    }

    /// Safety: This is unsafe and assumes the bytes are valid UTF-8. When creating a NoAllocString
    /// the bytes are validated to be UTF-8.
    pub fn as_str(&self) -> &str {
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
        self.as_str()
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
        assert_eq!(no_alloc_string.as_str(), "hello");
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
        assert_eq!(no_alloc_string.as_str(), "world");
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
        assert_eq!(no_alloc_string.as_str(), "unchecked");
    }

    #[test]
    fn test_as_str() {
        let no_alloc_string = NoAllocString::from_slice(b"test").unwrap();
        assert_eq!(no_alloc_string.as_str(), "test");
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
        assert_eq!(no_alloc_string.as_str(), "");
    }

    #[test]
    fn test_borrow() {
        let no_alloc_string = NoAllocString::from_slice(b"borrow").unwrap();
        let borrowed: &str = no_alloc_string.borrow();
        assert_eq!(borrowed, "borrow");
    }
}
