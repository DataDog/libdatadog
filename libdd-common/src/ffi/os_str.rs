// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A borrowed OS string backed by bytes, which need not be valid UTF-8.
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsStr([u8]);

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

impl AsRef<OsStr> for OsStr {
    fn as_ref(&self) -> &OsStr {
        self
    }
}

impl AsRef<OsStr> for str {
    fn as_ref(&self) -> &OsStr {
        OsStr::from_bytes(self.as_bytes())
    }
}
