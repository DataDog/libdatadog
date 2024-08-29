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

    pub fn create_no_alloc_string(&self, slice: &[u8]) -> NoAllocString {
        NoAllocString::from_bytes(self.buffer.slice_ref(slice).expect("Invalid slice"))
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
    pub fn from_slice(slice: &[u8]) -> NoAllocString {
        NoAllocString {
            bytes: tinybytes::Bytes::copy_from_slice(slice),
        }
    }
    // Creates a NoAllocString from a Bytes instance (does not copy the data)
    pub fn from_bytes(bytes: tinybytes::Bytes) -> NoAllocString {
        NoAllocString { bytes }
    }
    // pub fn as_str(&self) -> &str {
    //     std::str::from_utf8(&self.bytes).expect("Invalid UTF-8")
    // }
    // TODO: EK - Is this wise?
    pub fn as_str(&self) -> &str {
        // SAFETY: This is unsafe and assumes the bytes are valid UTF-8.
        // The caller must ensure that the bytes are valid UTF-8.
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
        let no_alloc_string = NoAllocString::from_slice(slice);
        assert_eq!(no_alloc_string.as_str(), "hello");
    }

    #[test]
    fn test_from_bytes() {
        let bytes = tinybytes::Bytes::copy_from_slice(b"world");
        let no_alloc_string = NoAllocString::from_bytes(bytes);
        assert_eq!(no_alloc_string.as_str(), "world");
    }

    #[test]
    fn test_as_str() {
        let no_alloc_string = NoAllocString::from_slice(b"test");
        assert_eq!(no_alloc_string.as_str(), "test");
    }

    #[test]
    fn test_serialize() {
        let no_alloc_string = NoAllocString::from_slice(b"serialize");
        let serialized = serde_json::to_string(&no_alloc_string).unwrap();
        assert_eq!(serialized, "\"serialize\"");
    }

    #[test]
    fn test_default() {
        let no_alloc_string: NoAllocString = Default::default();
        assert_eq!(no_alloc_string.as_str(), "");
    }

    #[test]
    fn test_borrow() {
        let no_alloc_string = NoAllocString::from_slice(b"borrow");
        let borrowed: &str = no_alloc_string.borrow();
        assert_eq!(borrowed, "borrow");
    }
}
