// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The writer lock between processes: an `fcntl` record lock on a private,
//! unlinked file.
//!
//! The kernel releases a process's record locks when it exits, however it
//! exits. That is what lets the next writer notice an interrupted insert (the
//! header's dirty marker) and recover, where an atomic in shared memory would
//! stay held forever.
//!
//! Record locks belong to processes, not threads or descriptors, so threads
//! of one process do not exclude each other: the table's thread lock does
//! that. Closing *any* descriptor for the file drops the process's locks, so
//! the descriptor is never duplicated or handed out.
//!
//! Windows tables are not shared between processes, and Miri cannot model
//! record locks, so both get a lock that always succeeds.

#[cfg(all(unix, not(miri)))]
pub use native::ProcessLock;
#[cfg(any(windows, miri))]
pub use stub::ProcessLock;

/// Why the process lock could not be taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(windows, miri),
    allow(dead_code, reason = "the stand-in lock never fails")
)]
pub enum LockError {
    /// Another process holds it.
    Busy,
    /// The lock file failed in a way retrying will not fix.
    Failed,
}

#[cfg(all(unix, not(miri)))]
mod native {
    use super::LockError;
    use crate::intern_table::InitError;
    use crate::intern_table::os::errno;
    use core::ffi::CStr;
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Makes lock file names unique within a process. A plain atomic, so no
    /// destructor runs at exit.
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    /// The longest lock path, including the terminating NUL.
    const PATH_BYTES: usize = 1024;
    const PREFIX: &[u8] = b"/ddprof-intern-";

    pub struct ProcessLock {
        fd: libc::c_int,
    }

    impl ProcessLock {
        /// Creates and unlinks `<dir>/ddprof-intern-<pid>-<serial>`, keeping
        /// only its descriptor. The directory is required.
        pub fn new(dir: Option<&CStr>) -> Result<Self, InitError> {
            let dir = dir.ok_or(InitError::InvalidConfig)?.to_bytes();
            let mut path = [0_u8; PATH_BYTES];
            // `<dir><PREFIX><pid: 8 hex>-<serial: 16 hex>`, then a NUL.
            let pid_at = dir.len().saturating_add(PREFIX.len());
            let serial_at = pid_at.saturating_add(8 + 1);
            let end = serial_at.saturating_add(16);
            if end >= PATH_BYTES {
                return Err(InitError::InvalidConfig);
            }
            put(&mut path, 0, dir);
            put(&mut path, dir.len(), PREFIX);
            // SAFETY: getpid has no arguments and cannot fail.
            let pid = u64::from(unsafe { libc::getpid() }.cast_unsigned());
            put(&mut path, pid_at, &hex::<8>(pid));
            put(&mut path, pid_at + 8, b"-");

            // O_EXCL makes the name unique (and refuses an existing symlink);
            // retry on a collision with a file left by another process.
            for _ in 0..32 {
                let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
                put(&mut path, serial_at, &hex::<16>(serial));
                // SAFETY: `path` is NUL-terminated: `end` is below its length
                // and nothing writes there. The mode lets only the owner in.
                let fd = unsafe {
                    libc::open(
                        path.as_ptr().cast(),
                        libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
                        libc::c_uint::from(0o600_u16),
                    )
                };
                if fd >= 0 {
                    let lock = Self { fd };
                    // SAFETY: as above. The descriptor keeps the file alive.
                    if unsafe { libc::unlink(path.as_ptr().cast()) } != 0 {
                        return Err(InitError::LockFailed);
                    }
                    return Ok(lock);
                }
                if errno() != libc::EEXIST {
                    return Err(InitError::LockFailed);
                }
            }
            Err(InitError::LockFailed)
        }

        pub fn try_acquire(&self) -> Result<(), LockError> {
            if self.set(libc::F_WRLCK) {
                return Ok(());
            }
            match errno() {
                libc::EACCES | libc::EAGAIN | libc::EINTR => Err(LockError::Busy),
                _ => Err(LockError::Failed),
            }
        }

        pub fn release(&self) -> Result<(), LockError> {
            if self.set(libc::F_UNLCK) {
                Ok(())
            } else {
                Err(LockError::Failed)
            }
        }

        /// Sets the lock on byte 0. `kind`'s type differs between platforms
        /// (`c_short` on macOS, `c_int` on Linux), as does `flock`'s field.
        fn set(&self, kind: impl TryInto<libc::c_short>) -> bool {
            // SAFETY: `flock` is plain integers, for which zero is valid.
            let mut range: libc::flock = unsafe { core::mem::zeroed() };
            let (Ok(kind), Ok(whence)) = (kind.try_into(), libc::c_short::try_from(libc::SEEK_SET))
            else {
                return false;
            };
            range.l_type = kind;
            range.l_whence = whence;
            range.l_len = 1;
            // SAFETY: the descriptor is open and `range` is initialized.
            // F_SETLK never waits. F_OFD_SETLK would be wrong: a child
            // inheriting the descriptor would share the parent's lock.
            unsafe { libc::fcntl(self.fd, libc::F_SETLK, &raw const range) == 0 }
        }
    }

    impl Drop for ProcessLock {
        fn drop(&mut self) {
            // SAFETY: this lock owns the descriptor in this process.
            unsafe { libc::close(self.fd) };
        }
    }

    /// Copies `bytes` into `path` at `at`, if they fit.
    fn put(path: &mut [u8], at: usize, bytes: &[u8]) {
        if let Some(dst) = at
            .checked_add(bytes.len())
            .and_then(|end| path.get_mut(at..end))
        {
            dst.copy_from_slice(bytes);
        }
    }

    /// `value`'s low `N` hex digits, most significant first.
    fn hex<const N: usize>(mut value: u64) -> [u8; N] {
        let mut out = [0; N];
        for byte in out.iter_mut().rev() {
            let [digit, ..] = (value & 0xf).to_le_bytes();
            *byte = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            };
            value >>= 4;
        }
        out
    }
}

#[cfg(any(windows, miri))]
mod stub {
    use super::LockError;
    use crate::intern_table::InitError;

    pub struct ProcessLock;

    impl ProcessLock {
        /// Miri runs the Unix code, which requires a directory; Windows
        /// tables are never shared, so they must not be given one.
        pub const fn new(dir: Option<&core::ffi::CStr>) -> Result<Self, InitError> {
            if dir.is_some() == cfg!(unix) {
                Ok(Self)
            } else {
                Err(InitError::InvalidConfig)
            }
        }

        #[allow(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "the same signature as the Unix lock"
        )]
        pub const fn try_acquire(&self) -> Result<(), LockError> {
            Ok(())
        }

        #[allow(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "the same signature as the Unix lock"
        )]
        pub const fn release(&self) -> Result<(), LockError> {
            Ok(())
        }
    }
}
