// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Names of the shared memory segments a sidecar and its processes open.

use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use zwohash::ZwoHasher;

/// `/{prefix}{namespace}{suffix}`, cut to the 31 bytes macOS allows for a shared memory name.
pub(crate) fn shm_path(prefix: &str, suffix: &str) -> CString {
    let mut path = format!("/{prefix}{}{suffix}", crate::shm_namespace());
    if cfg!(unix) {
        path.truncate(31);
    }
    // unwrap: CString::new only fails on a NUL byte; the namespace has none, and every caller
    // passes a literal prefix and either no suffix or a base64 one.
    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

/// [`shm_path`] for one segment per `key`, told apart by a hash of `key` that is the same in
/// every process.
pub(crate) fn keyed_shm_path(prefix: &str, key: &impl Hash) -> CString {
    let mut hasher = ZwoHasher::default();
    key.hash(&mut hasher);
    let hash = BASE64_URL_SAFE_NO_PAD.encode(hasher.finish().to_ne_bytes());
    shm_path(prefix, &format!("-{hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paths built here are the ones each caller used to build itself: a tuple key hashes
    /// like its fields hashed in turn.
    #[test]
    fn a_keyed_path_is_the_one_callers_built_before() {
        let (env, version, service) = ("env", "1.0", "service");
        let mut hasher = ZwoHasher::default();
        env.hash(&mut hasher);
        version.hash(&mut hasher);
        service.hash(&mut hasher);
        let mut before = format!(
            "/ddspsc{}-{}",
            crate::shm_namespace(),
            BASE64_URL_SAFE_NO_PAD.encode(hasher.finish().to_ne_bytes()),
        );
        if cfg!(unix) {
            before.truncate(31);
        }

        assert_eq!(
            keyed_shm_path("ddspsc", &(env, version, service)),
            CString::new(before).unwrap()
        );
    }

    #[test]
    fn a_path_fits_macos_names() {
        let path = keyed_shm_path("a-very-long-prefix-for-a-segment", &"key");
        if cfg!(unix) {
            assert_eq!(path.as_bytes().len(), 31);
        }
        assert!(path.as_bytes().starts_with(b"/a-very-long-prefix"));
    }
}
