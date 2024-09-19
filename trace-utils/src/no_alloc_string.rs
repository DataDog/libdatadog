use bytes::Bytes;
use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;

pub struct BufferWrapper {
    buffer: Bytes,
}

impl BufferWrapper {
    pub fn new(buffer: Bytes) -> Self {
        BufferWrapper { buffer }
    }

    pub fn create_no_alloc_string(&self, slice: &[u8]) -> NoAllocString {
        NoAllocString::from_bytes(self.buffer.slice_ref(slice))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NoAllocString {
    bytes: Bytes,
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
            bytes: Bytes::copy_from_slice(slice),
        }
    }
    // Creates a NoAllocString from a Bytes instance (does not copy the data)
    pub fn from_bytes(bytes: Bytes) -> NoAllocString {
        NoAllocString { bytes }
    }
    // TODO: Perhaps this should return the error instead of panicking
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("Invalid UTF-8")
    }
}

impl Default for NoAllocString {
    fn default() -> Self {
        NoAllocString {
            bytes: Bytes::new(),
        }
    }
}

impl Borrow<str> for NoAllocString {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
