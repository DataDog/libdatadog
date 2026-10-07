// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Ownership of shared-memory names.
//!
//! Always create with `O_CREAT | O_EXCL`. Reusing a segment would expose stale data and could
//! shrink the backing beneath existing readers. Instead, open and verify the old segment,
//! unlink it, and create a fresh segment. Return the old descriptors so the caller
//! can mark them obsolete once the replacement is initialized.
//!
//! On drop, remove the name only if it still identifies the creator's segment.

use super::{
    sys_create_exclusive, sys_identity, sys_open_existing, sys_shm_unlink, sys_stamp_identity,
};
use crate::platform::ShmPath;
use nix::errno::Errno;
use nix::sys::stat::Mode;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};

/// Bound retries if another process repeatedly recreates the name after unlink.
const MAX_REPLACEMENTS: usize = 4;

pub(crate) type SegmentIdentity = (u64, u64);

// Serialize name changes and reader detachment. A forked child can reclaim a lock
// held by a parent thread that no longer exists in the child.
static SHM_LOCK: AtomicU32 = AtomicU32::new(0);

pub(crate) struct ShmGuard;

pub(crate) fn lock_shm() -> ShmGuard {
    let pid = std::process::id();
    loop {
        match SHM_LOCK.compare_exchange(0, pid, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return ShmGuard,
            Err(holder) if holder != pid => {
                if SHM_LOCK
                    .compare_exchange(holder, pid, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return ShmGuard;
                }
            }
            Err(_) => std::thread::yield_now(),
        }
    }
}

impl Drop for ShmGuard {
    fn drop(&mut self) {
        SHM_LOCK.store(0, Ordering::Release);
    }
}

pub(crate) struct NameOwnership {
    /// Forked copies must leave the parent's name intact.
    pid: u32,
    identity: SegmentIdentity,
}

impl NameOwnership {
    pub(crate) fn release(&self, name: &CStr) {
        if self.pid != std::process::id() {
            return;
        }
        let _guard = lock_shm();
        if let Ok(fd) = sys_open_existing(name) {
            if sys_identity(&fd) == Some(self.identity) {
                _ = sys_shm_unlink(name);
            }
        }
    }
}

/// Create a fresh segment and return any verified, unlinked predecessors.
/// Run `init` and establish the new identity under the name lock; remove the name on failure.
/// The caller must initialize its payload before marking predecessors obsolete.
pub(crate) fn create_replacing(
    name: &CStr,
    mode: Mode,
    init: impl FnOnce(&OwnedFd) -> io::Result<()>,
) -> io::Result<(OwnedFd, ShmPath, Vec<OwnedFd>)> {
    let _guard = lock_shm();
    let mut predecessors = vec![];
    for _ in 0..MAX_REPLACEMENTS {
        match sys_create_exclusive(name, mode) {
            Ok(fd) => {
                let identity = init(&fd).and_then(|()| {
                    sys_stamp_identity(&fd).ok_or_else(|| {
                        io::Error::other(format!(
                            "could not establish the identity of shared memory {}",
                            name.to_string_lossy()
                        ))
                    })
                });
                return match identity {
                    Ok(identity) => {
                        let ownership = NameOwnership {
                            pid: std::process::id(),
                            identity,
                        };
                        let path = ShmPath {
                            name: name.to_owned(),
                            ownership: Some(ownership),
                        };
                        Ok((fd, path, predecessors))
                    }
                    Err(e) => {
                        _ = sys_shm_unlink(name);
                        Err(e)
                    }
                };
            }
            Err(Errno::EEXIST) => {}
            Err(e) => return Err(e.into()),
        }

        match sys_open_existing(name) {
            Ok(fd) => predecessors.push(fd),
            // Another process removed the name before we opened it.
            Err(Errno::ENOENT) => continue,
            Err(e) => return Err(e.into()),
        }
        match sys_shm_unlink(name) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "shared memory {} kept being recreated while replacing it",
            name.to_string_lossy()
        ),
    ))
}

/// Try unused names and unlink immediately, leaving access through the descriptor only.
/// Existing names are left untouched.
pub(crate) fn create_anonymous(
    mut candidate: impl FnMut() -> CString,
    mode: Mode,
) -> io::Result<OwnedFd> {
    const ATTEMPTS: usize = 16;
    for _ in 0..ATTEMPTS {
        let name = candidate();
        match sys_create_exclusive(&name, mode) {
            Ok(fd) => {
                _ = sys_shm_unlink(&name);
                return Ok(fd);
            }
            Err(Errno::EEXIST) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused name for an anonymous shared memory segment",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_name(tag: &str) -> CString {
        CString::new(format!("/ddtest-names-{}-{tag}", std::process::id())).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_failed_initialisation_leaves_no_name_behind() {
        let name = test_name("init");
        let result = create_replacing(&name, Mode::S_IRUSR | Mode::S_IWUSR, |_| {
            Err(io::Error::other("injected"))
        });
        assert!(result.is_err());
        assert_eq!(
            sys_open_existing(&name).err(),
            Some(Errno::ENOENT),
            "the half-made segment must be gone"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_failed_replacement_leaves_no_name_behind() {
        let name = test_name("replace");
        let old = crate::platform::NamedShmHandle::create(name.clone(), 16).unwrap();
        let result = create_replacing(&name, Mode::S_IRUSR | Mode::S_IWUSR, |_| {
            Err(io::Error::other("injected"))
        });
        assert!(result.is_err());
        assert_eq!(sys_open_existing(&name).err(), Some(Errno::ENOENT));
        drop(old);
    }
}
