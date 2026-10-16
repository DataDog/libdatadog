// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::{string::String, vec::Vec};
use core::{borrow::Borrow, ops::Deref};

/// An owned OS string backed by bytes, which need not be valid UTF-8.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsString(Vec<u8>);

/// A borrowed OS string backed by bytes, which need not be valid UTF-8.
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsStr([u8]);

impl OsString {
    /// Take ownership of arbitrary Unix-style OS bytes without copying.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Convert to UTF-8 without copying, returning the original bytes on failure.
    pub fn into_string(self) -> Result<String, Self> {
        String::from_utf8(self.0).map_err(|error| Self(error.into_bytes()))
    }
}

impl OsStr {
    /// Borrow arbitrary Unix-style OS bytes without copying.
    pub fn from_bytes(bytes: &[u8]) -> &Self {
        // SAFETY: OsStr is transparent over [u8] and imposes no byte validity constraints.
        unsafe { &*(bytes as *const [u8] as *const Self) }
    }

    /// Borrow the underlying encoded bytes.
    pub fn as_encoded_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for OsString {
    type Target = OsStr;

    fn deref(&self) -> &Self::Target {
        OsStr::from_bytes(&self.0)
    }
}

impl Borrow<OsStr> for OsString {
    fn borrow(&self) -> &OsStr {
        self
    }
}

impl AsRef<OsStr> for OsStr {
    fn as_ref(&self) -> &OsStr {
        self
    }
}

impl AsRef<OsStr> for OsString {
    fn as_ref(&self) -> &OsStr {
        self
    }
}

impl AsRef<OsStr> for str {
    fn as_ref(&self) -> &OsStr {
        OsStr::from_bytes(self.as_bytes())
    }
}

impl AsRef<OsStr> for String {
    fn as_ref(&self) -> &OsStr {
        self.as_str().as_ref()
    }
}

impl<T: AsRef<OsStr> + ?Sized> From<&T> for OsString {
    fn from(string: &T) -> Self {
        Self(string.as_ref().as_encoded_bytes().to_vec())
    }
}

impl From<String> for OsString {
    fn from(string: String) -> Self {
        Self(string.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{collections::BTreeMap, vec};

    #[test]
    fn utf8_conversions_preserve_the_owned_allocation() {
        let original = String::from("hello 🌍");
        let pointer = original.as_ptr();
        let string = OsString::from(original);
        assert_eq!(string.as_encoded_bytes().as_ptr(), pointer);
        let converted = string.into_string().unwrap();
        assert_eq!(converted.as_ptr(), pointer);
        assert_eq!(converted, "hello 🌍");
    }

    #[test]
    fn invalid_utf8_round_trips_without_loss() {
        let bytes = vec![b'a', 0xff, 0, b'b'];
        let pointer = bytes.as_ptr();
        let string = OsString::from_vec(bytes);
        let string = string.into_string().unwrap_err();
        assert_eq!(string.as_encoded_bytes().as_ptr(), pointer);
        assert_eq!(string.as_encoded_bytes(), [b'a', 0xff, 0, b'b']);
    }

    #[test]
    fn borrowed_keys_use_the_same_ordering_as_owned_keys() {
        let mut values = BTreeMap::new();
        values.insert(OsString::from_vec(vec![0xff]), 1);
        values.insert(OsString::from("service"), 2);
        assert_eq!(values.get(OsStr::from_bytes(&[0xff])), Some(&1));
        assert_eq!(values.get::<OsStr>("service".as_ref()), Some(&2));
        assert_eq!(values.get::<OsStr>("missing".as_ref()), None);
    }

    #[cfg(all(feature = "std", unix))]
    #[test]
    fn conversions_match_native_unix_os_strings() {
        use std::os::unix::ffi::OsStringExt;

        for bytes in [b"".as_slice(), b"plain", "hello 🌍".as_bytes(), b"a\xff\0b"] {
            let native = std::ffi::OsString::from_vec(bytes.to_vec());
            let ours = OsString::from_vec(bytes.to_vec());
            assert_eq!(
                ours.into_string().map_err(|value| value.0),
                native.into_string().map_err(std::ffi::OsString::into_vec)
            );
        }
    }
}
