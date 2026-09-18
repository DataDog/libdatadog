// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Who is allowed to own a shared-memory segment we map.
//!
//! Every check here takes a **descriptor**, never a name. `fstat` describes the object that will
//! actually be mapped, so unlike a name-based check there is no window in which the name could
//! be repointed at something else in between.

use nix::errno::Errno;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering};

/// Sentinel meaning "no owner uid override".
const NO_OWNER_UID: u32 = u32::MAX;

static SHM_OWNER_UID: AtomicU32 = AtomicU32::new(NO_OWNER_UID);

/// Declare the uid that segments are handed to, in addition to this process's own.
///
/// E.g. a sidecar running as root serves workers that dropped privileges to a service account; it
/// `fchown`s the segments it creates so those workers can map them, and records the uid here so
/// that reading such a segment back is not mistaken for somebody else's.
pub fn set_shm_owner_uid(uid: u32) {
    SHM_OWNER_UID.store(uid, Ordering::Relaxed);
}

pub(crate) fn shm_owner_uid() -> Option<u32> {
    let uid = SHM_OWNER_UID.load(Ordering::Relaxed);
    if uid == NO_OWNER_UID {
        None
    } else {
        Some(uid)
    }
}

/// Refuse a segment that some other user could have created.
///
/// Acceptable owners are this euid, root - which can read this process's memory regardless, so
/// refusing it protects nothing - and the uid declared by [`set_shm_owner_uid`].
pub(crate) fn verify_owner<F: FnOnce() -> String>(fd: &impl AsRawFd, name: F) -> nix::Result<()> {
    let uid = nix::sys::stat::fstat(fd.as_raw_fd())?.st_uid;
    if owner_is_acceptable(uid, current_uid(), shm_owner_uid()) {
        return Ok(());
    }
    let name = name();
    tracing::error!(
        "Refusing shared memory {name}: owned by uid {uid}, expected {}. Another user may be \
         squatting this name.",
        current_uid()
    );
    Err(Errno::EPERM)
}

/// The rule [`verify_owner`] applies, split out so it can be tested for uids this process
/// cannot actually create a file as.
fn owner_is_acceptable(owner: u32, euid: u32, declared: Option<u32>) -> bool {
    owner == euid || owner == 0 || declared == Some(owner)
}

fn current_uid() -> u32 {
    // SAFETY: geteuid() takes no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn accepts_a_segment_we_own() {
        #[allow(clippy::unwrap_used)]
        let file = tempfile::tempfile().unwrap();
        verify_owner(&file.as_fd(), || "own".to_string())
            .expect("a file we created must be accepted");
    }

    #[test]
    fn foreign_owners_are_refused() {
        // 0 is root, 500 stands in for "us"; anything else is somebody who should not be able
        // to hand us memory. This is the case a test cannot stage for real without privileges.
        assert!(owner_is_acceptable(500, 500, None), "our own uid");
        assert!(owner_is_acceptable(0, 500, None), "root can read us anyway");
        assert!(!owner_is_acceptable(501, 500, None), "another user");
        assert!(
            owner_is_acceptable(33, 0, Some(33)),
            "the uid a root sidecar handed its segments to"
        );
        assert!(
            !owner_is_acceptable(34, 0, Some(33)),
            "a declared owner must not widen access to any other uid"
        );
    }

    #[test]
    fn owner_uid_override_round_trips() {
        assert_eq!(shm_owner_uid(), None, "no override by default");
        set_shm_owner_uid(4242);
        assert_eq!(shm_owner_uid(), Some(4242));
        set_shm_owner_uid(NO_OWNER_UID);
        assert_eq!(shm_owner_uid(), None);
    }
}
