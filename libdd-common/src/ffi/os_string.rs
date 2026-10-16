// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::{borrow::Cow, borrow::ToOwned, string::String, vec::Vec};
use core::{borrow::Borrow, fmt, ops::Deref};

/// An owned OS string backed by bytes, which need not be valid UTF-8.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OsString(Vec<u8>);

/// A borrowed OS string backed by bytes, which need not be valid UTF-8.
#[repr(transparent)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OsStr([u8]);

impl OsString {
    /// Construct an empty string without allocating.
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Construct an empty string with space for at least `capacity` bytes.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Take ownership of arbitrary Unix-style OS bytes without copying.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the whole string.
    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }

    /// Consume the string and return its bytes without copying.
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    /// Consume the string and return its encoded bytes without copying.
    pub fn into_encoded_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Convert to UTF-8 without copying, returning the original bytes on failure.
    pub fn into_string(self) -> Result<String, Self> {
        String::from_utf8(self.0).map_err(|error| Self(error.into_bytes()))
    }

    /// Append another OS string.
    pub fn push<S: AsRef<OsStr>>(&mut self, string: S) {
        self.0.extend_from_slice(string.as_ref().as_bytes());
    }

    /// Remove all bytes while retaining the allocation.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Return the allocation's capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl OsStr {
    /// Borrow a string or OS string without copying.
    pub fn new<S: AsRef<Self> + ?Sized>(string: &S) -> &Self {
        string.as_ref()
    }

    /// Borrow arbitrary Unix-style OS bytes without copying.
    pub fn from_bytes(bytes: &[u8]) -> &Self {
        // SAFETY: OsStr is transparent over [u8] and imposes no byte validity constraints.
        unsafe { &*(bytes as *const [u8] as *const Self) }
    }

    /// Borrow the underlying bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Borrow the underlying encoded bytes.
    pub fn as_encoded_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Borrow the string as UTF-8, or return `None` if any bytes are invalid.
    pub fn to_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.0).ok()
    }

    /// Borrow valid UTF-8 or replace invalid sequences with U+FFFD in an owned string.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// Copy the string into owned storage.
    pub fn to_os_string(&self) -> OsString {
        OsString(self.0.to_vec())
    }

    /// Return the number of bytes in the string.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the string contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Deref for OsString {
    type Target = OsStr;

    fn deref(&self) -> &Self::Target {
        self.as_os_str()
    }
}

impl Borrow<OsStr> for OsString {
    fn borrow(&self) -> &OsStr {
        self.as_os_str()
    }
}

impl ToOwned for OsStr {
    type Owned = OsString;

    fn to_owned(&self) -> Self::Owned {
        self.to_os_string()
    }
}

impl AsRef<OsStr> for OsStr {
    fn as_ref(&self) -> &OsStr {
        self
    }
}

impl AsRef<OsStr> for OsString {
    fn as_ref(&self) -> &OsStr {
        self.as_os_str()
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
        string.as_ref().to_os_string()
    }
}

impl From<String> for OsString {
    fn from(string: String) -> Self {
        Self(string.into_bytes())
    }
}

impl fmt::Debug for OsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for byte in self.0.escape_ascii() {
            write!(f, "{}", char::from(byte))?;
        }
        f.write_str("\"")
    }
}

impl fmt::Debug for OsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_os_str().fmt(f)
    }
}

macro_rules! string_eq {
    ($left:ty, $right:ty) => {
        impl PartialEq<$right> for $left {
            fn eq(&self, other: &$right) -> bool {
                AsRef::<OsStr>::as_ref(self).as_bytes() == AsRef::<OsStr>::as_ref(other).as_bytes()
            }
        }
        impl PartialEq<$left> for $right {
            fn eq(&self, other: &$left) -> bool {
                other == self
            }
        }
    };
}

string_eq!(OsString, OsStr);
string_eq!(OsString, &OsStr);
string_eq!(OsString, str);
string_eq!(OsString, &str);
string_eq!(OsString, String);
string_eq!(OsStr, str);
string_eq!(OsStr, &str);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{collections::BTreeMap, format, vec};

    #[test]
    fn utf8_conversions_preserve_the_owned_allocation() {
        let original = String::from("hello 🌍");
        let pointer = original.as_ptr();
        let string = OsString::from(original);
        assert_eq!(string.as_bytes().as_ptr(), pointer);
        assert_eq!(string, "hello 🌍");
        assert!(matches!(string.to_string_lossy(), Cow::Borrowed(_)));
        let converted = string.into_string().unwrap();
        assert_eq!(converted.as_ptr(), pointer);
        assert_eq!(converted, "hello 🌍");
    }

    #[test]
    fn invalid_utf8_round_trips_without_loss() {
        let bytes = vec![b'a', 0xff, 0, b'b'];
        let pointer = bytes.as_ptr();
        let string = OsString::from_vec(bytes);
        assert_eq!(string.to_str(), None);
        assert_eq!(string.to_string_lossy(), "a\u{fffd}\0b");
        let string = string.into_string().unwrap_err();
        assert_eq!(string.as_bytes().as_ptr(), pointer);
        assert_eq!(string.into_vec(), [b'a', 0xff, 0, b'b']);
    }

    #[test]
    fn borrowed_keys_use_the_same_ordering_as_owned_keys() {
        let mut values = BTreeMap::new();
        values.insert(OsString::from_vec(vec![0xff]), 1);
        values.insert(OsString::from("service"), 2);
        assert_eq!(values.get(OsStr::from_bytes(&[0xff])), Some(&1));
        assert_eq!(values.get(OsStr::new("service")), Some(&2));
        assert_eq!(values.get(OsStr::new("missing")), None);
    }

    #[test]
    fn borrowed_strings_and_mutation_preserve_bytes() {
        let bytes = [b'a', 0xff];
        let borrowed = OsStr::from_bytes(&bytes);
        assert_eq!(borrowed.as_bytes().as_ptr(), bytes.as_ptr());
        let mut owned = OsString::with_capacity(20);
        owned.push(borrowed);
        owned.push("tail");
        assert_eq!(owned.as_encoded_bytes(), b"a\xfftail");
        assert_eq!(owned.len(), 6);
        let capacity = owned.capacity();
        owned.clear();
        assert!(owned.is_empty());
        assert_eq!(owned.capacity(), capacity);
        assert_eq!(borrowed.to_owned().into_encoded_bytes(), bytes);
        assert!(OsString::new().is_empty());
    }

    #[test]
    fn debug_escapes_invalid_and_control_bytes() {
        assert_eq!(
            format!("{:?}", OsStr::from_bytes(b"a\xff\n\"")),
            "\"a\\xff\\n\\\"\""
        );
    }

    #[cfg(all(feature = "std", unix))]
    #[test]
    fn conversions_match_native_unix_os_strings() {
        use std::os::unix::ffi::OsStringExt;

        for bytes in [b"".as_slice(), b"plain", "hello 🌍".as_bytes(), b"a\xff\0b"] {
            let native = std::ffi::OsString::from_vec(bytes.to_vec());
            let ours = OsString::from_vec(bytes.to_vec());
            assert_eq!(ours.to_str(), native.to_str());
            assert_eq!(ours.to_string_lossy(), native.to_string_lossy());
            assert_eq!(
                ours.into_string().map_err(OsString::into_vec),
                native.into_string().map_err(std::ffi::OsString::into_vec)
            );
        }
    }
}
