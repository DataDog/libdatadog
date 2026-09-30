// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Constrained opens for paths supplied by workers.
//!
//! A sidecar thread that drops to a worker's UID still shares the PHP master's address space and
//! file descriptors. Paths such as `/proc/self/maps` and `/dev/fd/N` can expose the master's memory
//! layout or bypass directory permissions. Ordinary symlinks can lead to these paths too.
//!
//! On Linux, resolve paths with `openat2(RESOLVE_NO_MAGICLINKS)` and pin the target with `O_PATH`.
//! Reject procfs/sysfs and check the object type before opening it for I/O, so device drivers are
//! never invoked. Reads follow ordinary symlinks; writes refuse a symlink as the final component.
//!
//! When `openat2` is unavailable or blocked by seccomp, walk one component at a time with
//! `O_PATH | O_NOFOLLOW`, checking each pinned object before following symlinks. For Linux 3.10,
//! where `fstatfs` rejects `O_PATH`, inspect the pinned object through our own `/proc/self/fd`
//! entry.
//!
//! On macOS, `/dev/fd/N` is not a symlink. Open with `O_NOFOLLOW`, get the real path with
//! `F_GETPATH`, then reopen that path to check directory permissions and verify that the inode
//! still matches. This also refuses ordinary symlinks as the final component.

// File and syscall operations here require std.
#![allow(clippy::std_instead_of_alloc, clippy::std_instead_of_core)]
// `statfs::f_type` and the `*_MAGIC` constants have different integer types across libc targets
// (gnu vs musl), so the `as i64` normalisation is a no-op only on some of them.
#![allow(clippy::unnecessary_cast)]

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide restrictions for paths supplied by workers. The thread listener enables this
/// after dropping to a different UID; individual requests cannot select this policy. Crash
/// attachments also carry an explicit policy per report.
static RESTRICT_WORKER_FILE_OUTPUTS: AtomicBool = AtomicBool::new(false);

/// Set the process-wide policy for worker paths.
pub fn set_restrict_worker_file_outputs(on: bool) {
    RESTRICT_WORKER_FILE_OUTPUTS.store(on, Ordering::Relaxed);
}

/// Whether paths supplied by workers need constrained opens in this process.
pub fn worker_file_outputs_restricted() -> bool {
    RESTRICT_WORKER_FILE_OUTPUTS.load(Ordering::Relaxed)
}

/// Why an open was refused. Diagnostics must not reveal the target's contents or resolved path.
#[derive(Debug, thiserror::Error)]
pub enum RestrictedOpenError {
    /// The platform cannot enforce the restriction. Never retry with an unrestricted open.
    #[error("constrained file access is not supported on this kernel")]
    Unsupported,
    /// The target was refused by policy.
    #[error("refused worker-selected path: {0}")]
    Rejected(&'static str),
    /// A syscall failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<RestrictedOpenError> for io::Error {
    fn from(value: RestrictedOpenError) -> Self {
        match value {
            RestrictedOpenError::Io(e) => e,
            RestrictedOpenError::Unsupported => {
                io::Error::new(io::ErrorKind::Unsupported, value.to_string())
            }
            RestrictedOpenError::Rejected(_) => {
                io::Error::new(io::ErrorKind::PermissionDenied, value.to_string())
            }
        }
    }
}

type Result<T> = std::result::Result<T, RestrictedOpenError>;

/// Require an absolute path: relative paths use the master's working directory, which may be
/// inside a directory the worker cannot traverse.
fn checked_cpath(path: &Path) -> Result<CString> {
    if !path.is_absolute() {
        return Err(RestrictedOpenError::Rejected("relative worker path"));
    }
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| RestrictedOpenError::Rejected("path contains an interior NUL"))
}

/// Open a regular file for reading, refusing magic links, procfs and sysfs.
pub fn open_regular_for_read(path: &Path) -> Result<File> {
    platform::open_regular_for_read(&checked_cpath(path)?)
}

/// Open or create a regular file for appending. Refuse magic links, procfs/sysfs and a symlink as
/// the final component. Use exclusive creation so a file planted after inspection is not opened.
pub fn open_regular_for_append(path: &Path) -> Result<File> {
    platform::open_regular_for_write(path, &checked_cpath(path)?, WriteMode::Append)
}

/// Create or truncate a regular file, with the same restrictions as [`open_regular_for_append`].
pub fn open_regular_for_create(path: &Path) -> Result<File> {
    platform::open_regular_for_write(path, &checked_cpath(path)?, WriteMode::Truncate)
}

#[derive(Clone, Copy)]
enum WriteMode {
    Append,
    Truncate,
}

/// Validate an absolute Unix-socket path and return a handle and its `/proc/self/fd` path.
/// Keep the handle alive for every use of that path, including datagram sends, so it continues to
/// name the validated socket. Refuse magic links, procfs/sysfs and non-socket targets.
///
/// Supported on Linux only. Callers handle abstract sockets separately, without filesystem checks.
#[cfg(target_os = "linux")]
pub fn constrained_unix_socket_fd(
    path: &Path,
) -> Result<(std::os::fd::OwnedFd, std::path::PathBuf)> {
    platform::constrained_unix_socket_fd(&checked_cpath(path)?)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn constrained_unix_socket_fd(
    _path: &Path,
) -> Result<(std::os::fd::OwnedFd, std::path::PathBuf)> {
    Err(RestrictedOpenError::Unsupported)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{RestrictedOpenError, Result};
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    /// Prefer `openat2`, falling back to a constrained walk on older kernels or when seccomp
    /// blocks this syscall. All callers request `O_PATH`; this never opens a target for I/O.
    fn open_path(
        dir_fd: RawFd,
        path: &CStr,
        flags: libc::c_int,
        reject_msg: &'static str,
    ) -> Result<OwnedFd> {
        // open_how is #[non_exhaustive]; zero-initialise then set the public fields.
        // SAFETY: open_how is a plain POD struct; an all-zero value is valid.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_CLOEXEC) as u64;
        how.resolve = libc::RESOLVE_NO_MAGICLINKS;

        // SAFETY: `path` is a valid NUL-terminated C string, `&how` points to a correctly sized
        // open_how, and the size argument matches. openat2 returns a new fd or -1.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dir_fd,
                path.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if rc >= 0 {
            // SAFETY: rc is a fresh, owned fd from openat2.
            return Ok(unsafe { OwnedFd::from_raw_fd(rc as RawFd) });
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENOSYS) | Some(libc::EPERM) => open_path_fallback(dir_fd, path, flags),
            Some(libc::ELOOP) => Err(RestrictedOpenError::Rejected(reject_msg)),
            _ => Err(RestrictedOpenError::Io(err)),
        }
    }

    fn open_component(dir_fd: RawFd, path: &CStr, flags: libc::c_int) -> Result<OwnedFd> {
        // Callers pass a single component (or "/" for the root). O_NOFOLLOW therefore prevents
        // every automatic symlink traversal, including magic links, without opening devices.
        // SAFETY: path is NUL-terminated; openat returns a new owned fd or -1.
        let fd = unsafe {
            libc::openat(
                dir_fd,
                path.as_ptr(),
                flags | libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: fd is a fresh, owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn open_path_fallback(dir_fd: RawFd, path: &CStr, flags: libc::c_int) -> Result<OwnedFd> {
        // Bound both work and storage for attacker-controlled paths and symlink chains.
        const MAX_PATH: usize = libc::PATH_MAX as usize;
        const MAX_SYMLINKS: usize = 40;
        let bytes = path.to_bytes();
        if bytes.is_empty() {
            return Err(RestrictedOpenError::Io(std::io::Error::from_raw_os_error(
                libc::ENOENT,
            )));
        }
        if bytes.len() >= MAX_PATH {
            return Err(RestrictedOpenError::Io(std::io::Error::from_raw_os_error(
                libc::ENAMETOOLONG,
            )));
        }
        let mut pending = bytes.to_vec();
        let start = if pending[0] == b'/' { c"/" } else { c"." };
        let mut current = open_component(dir_fd, start, libc::O_DIRECTORY)?;
        reject_pseudo_filesystem(current.as_raw_fd())?;
        let mut offset = 0;
        let mut symlinks = 0;
        while offset < pending.len() {
            if pending[offset] == b'/' {
                offset += 1;
                continue;
            }
            let end = pending[offset..]
                .iter()
                .position(|&b| b == b'/')
                .map_or(pending.len(), |n| offset + n);
            let component = CString::new(&pending[offset..end])
                .map_err(|_| RestrictedOpenError::Rejected("path contains an interior NUL"))?;
            let next = open_component(current.as_raw_fd(), &component, 0)?;
            reject_pseudo_filesystem(next.as_raw_fd())?;
            let kind = file_type(next.as_raw_fd())?;
            // Keep separators in the remaining path: a trailing slash requires a directory,
            // and "file/.." must fail instead of being lexically simplified away.
            let has_suffix = end < pending.len();
            if kind == libc::S_IFLNK && (has_suffix || flags & libc::O_NOFOLLOW == 0) {
                symlinks += 1;
                if symlinks > MAX_SYMLINKS {
                    return Err(RestrictedOpenError::Rejected("too many symlinks"));
                }
                let mut target = vec![0u8; MAX_PATH];
                // SAFETY: next pins this exact symlink; the empty path reads that handle, not
                // a name an attacker can swap. readlinkat never follows the link for us.
                let len = unsafe {
                    libc::readlinkat(
                        next.as_raw_fd(),
                        c"".as_ptr(),
                        target.as_mut_ptr().cast(),
                        target.len(),
                    )
                };
                if len < 0 {
                    return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
                }
                // SAFETY of the conversion: a nonnegative readlinkat result fits in usize.
                let len = len as usize;
                if len == 0 || len + pending.len() - end >= MAX_PATH {
                    return Err(RestrictedOpenError::Rejected(
                        "invalid or overlong symlink target",
                    ));
                }
                target.truncate(len);
                target.extend_from_slice(&pending[end..]);
                if target[0] == b'/' {
                    current = open_component(libc::AT_FDCWD, c"/", libc::O_DIRECTORY)?;
                    reject_pseudo_filesystem(current.as_raw_fd())?;
                }
                pending = target;
                offset = 0;
                continue;
            }
            if (has_suffix || flags & libc::O_DIRECTORY != 0) && kind != libc::S_IFDIR {
                return Err(RestrictedOpenError::Io(std::io::Error::from_raw_os_error(
                    libc::ENOTDIR,
                )));
            }
            current = next;
            offset = end;
        }
        Ok(current)
    }

    fn proc_fd_path(fd: RawFd) -> Result<CString> {
        CString::new(format!("/proc/self/fd/{fd}"))
            .map_err(|_| RestrictedOpenError::Rejected("fd path contains an interior NUL"))
    }

    /// Procfs/sysfs can expose process state through ordinary-looking regular files.
    fn reject_pseudo_filesystem(fd: RawFd) -> Result<()> {
        // SAFETY: statfs is POD; fstatfs fills it or returns -1.
        let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatfs(fd, &mut sfs) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EBADF) {
                return Err(RestrictedOpenError::Io(err));
            }
            // Before Linux 3.12, fstatfs rejects O_PATH. Our live descriptor still pins the object,
            // so statfs through its proc entry is safe after a rename or unlink. For an O_NOFOLLOW
            // symlink handle, this inspects the symlink's filesystem without following its target.
            let proc_path = proc_fd_path(fd)?;
            // SAFETY: proc_path is NUL-terminated and sfs is writable.
            if unsafe { libc::statfs(proc_path.as_ptr(), &mut sfs) } != 0 {
                return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
            }
        }
        let f_type = sfs.f_type as i64;
        if f_type == libc::PROC_SUPER_MAGIC as i64 {
            return Err(RestrictedOpenError::Rejected("procfs path"));
        }
        if f_type == libc::SYSFS_MAGIC as i64 {
            return Err(RestrictedOpenError::Rejected("sysfs path"));
        }
        Ok(())
    }

    fn file_type(fd: RawFd) -> Result<libc::mode_t> {
        // SAFETY: stat is POD; fstat fills it or returns -1.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstat(fd, &mut st) };
        if rc != 0 {
            return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
        }
        Ok(st.st_mode & libc::S_IFMT)
    }

    fn require_regular_file(fd: RawFd) -> Result<()> {
        if file_type(fd)? != libc::S_IFREG {
            return Err(RestrictedOpenError::Rejected("not a regular file"));
        }
        Ok(())
    }

    /// Open the validated regular file for I/O through our own live descriptor. The pinned handle
    /// keeps a pathname swap from changing the target.
    fn reopen_opath(opath: &OwnedFd, flags: libc::c_int) -> Result<File> {
        let proc_path = proc_fd_path(opath.as_raw_fd())?;
        // SAFETY: proc_path is a valid C string; open returns a new fd or -1.
        let rc = unsafe { libc::open(proc_path.as_ptr(), flags | libc::O_CLOEXEC) };
        if rc < 0 {
            return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: rc is a fresh, owned fd.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(rc) }))
    }

    pub(super) fn constrained_unix_socket_fd(
        cpath: &CStr,
    ) -> Result<(OwnedFd, std::path::PathBuf)> {
        let opath = open_path(
            libc::AT_FDCWD,
            cpath,
            libc::O_PATH,
            "magic link in socket path",
        )?;
        reject_pseudo_filesystem(opath.as_raw_fd())?;
        if file_type(opath.as_raw_fd())? != libc::S_IFSOCK {
            return Err(RestrictedOpenError::Rejected("not a socket"));
        }
        let connect_path = std::path::PathBuf::from(format!("/proc/self/fd/{}", opath.as_raw_fd()));
        Ok((opath, connect_path))
    }

    pub(super) fn open_regular_for_read(cpath: &CStr) -> Result<File> {
        let opath = open_path(libc::AT_FDCWD, cpath, libc::O_PATH, "magic link in path")?;
        reject_pseudo_filesystem(opath.as_raw_fd())?;
        require_regular_file(opath.as_raw_fd())?;
        reopen_opath(&opath, libc::O_RDONLY)
    }

    pub(super) fn open_regular_for_write(
        path: &std::path::Path,
        _cpath: &CStr,
        mode: super::WriteMode,
    ) -> Result<File> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let file_name = path.file_name().ok_or(RestrictedOpenError::Rejected(
            "output path has no file name",
        ))?;
        let cparent = CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| RestrictedOpenError::Rejected("path contains an interior NUL"))?;
        let cname = CString::new(file_name.as_bytes())
            .map_err(|_| RestrictedOpenError::Rejected("path contains an interior NUL"))?;

        let write_flag = match mode {
            super::WriteMode::Append => libc::O_APPEND,
            super::WriteMode::Truncate => libc::O_TRUNC,
        };

        // Pin the validated parent so a rename or symlink swap cannot redirect the final open.
        let dir = open_path(
            libc::AT_FDCWD,
            &cparent,
            libc::O_PATH | libc::O_DIRECTORY,
            "magic link in output directory",
        )?;
        reject_pseudo_filesystem(dir.as_raw_fd())?;

        // Inspect without following the final symlink or opening devices. If the file is absent,
        // create it exclusively to prevent another file from being planted after this check.
        let inspect = open_path(
            dir.as_raw_fd(),
            &cname,
            libc::O_PATH | libc::O_NOFOLLOW,
            "magic link as output file",
        );
        match inspect {
            Ok(existing) => {
                reject_pseudo_filesystem(existing.as_raw_fd())?;
                // O_PATH | O_NOFOLLOW pins the symlink itself, so the type check rejects it.
                require_regular_file(existing.as_raw_fd())?;
                reopen_opath(&existing, libc::O_WRONLY | write_flag)
            }
            Err(RestrictedOpenError::Io(e)) if e.raw_os_error() == Some(libc::ENOENT) => {
                // SAFETY: cname is a valid C string relative to the O_PATH directory handle.
                let rc = unsafe {
                    libc::openat(
                        dir.as_raw_fd(),
                        cname.as_ptr(),
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | write_flag
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        0o600,
                    )
                };
                if rc < 0 {
                    return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
                }
                // SAFETY: rc is a fresh, owned fd.
                Ok(File::from(unsafe { OwnedFd::from_raw_fd(rc) }))
            }
            Err(other) => Err(other),
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod platform {
    use super::{RestrictedOpenError, Result};
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    // O_NOFOLLOW does not stop macOS descriptor aliases such as /dev/fd/N. Reopening the path from
    // F_GETPATH checks directory permissions; comparing device and inode numbers detects a swap.

    fn fstat_regular(fd: RawFd) -> Result<libc::stat> {
        // SAFETY: stat is POD; fstat fills it or returns -1.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
        }
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(RestrictedOpenError::Rejected("not a regular file"));
        }
        Ok(st)
    }

    fn same_object(a: &libc::stat, b: &libc::stat) -> bool {
        a.st_dev == b.st_dev && a.st_ino == b.st_ino
    }

    /// Get the target's filesystem path so reopening it checks directory permissions.
    fn canonical_path(fd: RawFd) -> Result<CString> {
        let mut buf = [0 as libc::c_char; libc::PATH_MAX as usize];
        // SAFETY: buf holds PATH_MAX bytes; F_GETPATH writes a NUL-terminated path into it.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
            return Err(RestrictedOpenError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: F_GETPATH NUL-terminates within PATH_MAX.
        Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_owned())
    }

    fn open_nofollow(cpath: &CStr, flags: libc::c_int) -> Result<OwnedFd> {
        // O_NONBLOCK avoids waiting on a FIFO before we can check its type.
        let full = flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        let rc = unsafe { libc::open(cpath.as_ptr(), full, 0o600 as libc::c_int) };
        if rc >= 0 {
            // SAFETY: rc is a fresh, owned fd.
            return Ok(unsafe { OwnedFd::from_raw_fd(rc) });
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ELOOP) => Err(RestrictedOpenError::Rejected("symlink final component")),
            _ => Err(RestrictedOpenError::Io(err)),
        }
    }

    /// Reopen the canonical path and check that it still names the same regular file.
    fn reopen_canonical(fd: &OwnedFd, st1: &libc::stat, flags: libc::c_int) -> Result<File> {
        let canonical = canonical_path(fd.as_raw_fd())?;
        let reopened = open_nofollow(&canonical, flags)?;
        let st2 = fstat_regular(reopened.as_raw_fd())?;
        if !same_object(st1, &st2) {
            return Err(RestrictedOpenError::Rejected(
                "target changed identity during resolution",
            ));
        }
        Ok(File::from(reopened))
    }

    pub(super) fn open_regular_for_read(cpath: &CStr) -> Result<File> {
        let fd = open_nofollow(cpath, libc::O_RDONLY)?;
        let st1 = fstat_regular(fd.as_raw_fd())?;
        reopen_canonical(&fd, &st1, libc::O_RDONLY)
    }

    pub(super) fn open_regular_for_write(
        _path: &std::path::Path,
        cpath: &CStr,
        mode: super::WriteMode,
    ) -> Result<File> {
        let write_flag = match mode {
            super::WriteMode::Append => libc::O_APPEND,
            super::WriteMode::Truncate => libc::O_TRUNC,
        };
        // Inspect read-only first, so descriptor aliases are resolved before opening for writes.
        match open_nofollow(cpath, libc::O_RDONLY) {
            Ok(fd) => {
                let st1 = fstat_regular(fd.as_raw_fd())?;
                reopen_canonical(&fd, &st1, libc::O_WRONLY | write_flag)
            }
            Err(RestrictedOpenError::Io(e)) if e.raw_os_error() == Some(libc::ENOENT) => {
                // Exclusive creation refuses a file or symlink planted after inspection.
                let fd = open_nofollow(
                    cpath,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | write_flag,
                )?;
                let _ = fstat_regular(fd.as_raw_fd())?;
                Ok(File::from(fd))
            }
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::symlink;

    fn tmp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ddrestrict-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn read_all(path: &std::path::Path) -> Vec<u8> {
        let mut buf = Vec::new();
        open_regular_for_read(path)
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        buf
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reads_an_ordinary_file() {
        let dir = tmp_dir();
        let path = dir.join("ordinary.txt");
        std::fs::write(&path, b"hello canary\n").unwrap();
        assert_eq!(read_all(&path), b"hello canary\n");
    }

    // The macOS fallback refuses symlinks as the final component.
    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn follows_an_ordinary_symlink_to_a_regular_file() {
        let dir = tmp_dir();
        let target = dir.join("real.txt");
        std::fs::write(&target, b"through symlink\n").unwrap();
        let link = dir.join("link.txt");
        let _ = std::fs::remove_file(&link);
        symlink(&target, &link).unwrap();
        assert_eq!(read_all(&link), b"through symlink\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_procfs_regular_file() {
        // This is a regular file, not a magic link.
        let err = open_regular_for_read(std::path::Path::new("/proc/self/status")).unwrap_err();
        assert!(
            matches!(
                err,
                RestrictedOpenError::Rejected(_) | RestrictedOpenError::Unsupported
            ),
            "unexpected: {err:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_proc_self_fd_magic_link() {
        let dir = tmp_dir();
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, b"SECRET\n").unwrap();
        let held = std::fs::File::open(&secret).unwrap();
        let via_fd = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&held));
        let err = open_regular_for_read(std::path::Path::new(&via_fd)).unwrap_err();
        assert!(
            matches!(
                err,
                RestrictedOpenError::Rejected(_) | RestrictedOpenError::Unsupported
            ),
            "unexpected: {err:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_symlink_to_proc_self_fd() {
        let dir = tmp_dir();
        let secret = dir.join("secret2.txt");
        std::fs::write(&secret, b"SECRET2\n").unwrap();
        let held = std::fs::File::open(&secret).unwrap();
        let alias = dir.join("fd-alias");
        let _ = std::fs::remove_file(&alias);
        symlink(
            format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&held)),
            &alias,
        )
        .unwrap();
        let err = open_regular_for_read(&alias).unwrap_err();
        assert!(
            matches!(
                err,
                RestrictedOpenError::Rejected(_) | RestrictedOpenError::Unsupported
            ),
            "unexpected: {err:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_a_fifo() {
        let dir = tmp_dir();
        let fifo = dir.join("fifo");
        let _ = std::fs::remove_file(&fifo);
        let cfifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cfifo.as_ptr(), 0o600) }, 0);
        let err = open_regular_for_read(&fifo).unwrap_err();
        assert!(
            matches!(
                err,
                RestrictedOpenError::Rejected(_) | RestrictedOpenError::Unsupported
            ),
            "unexpected: {err:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn append_writes_to_an_ordinary_file() {
        let dir = tmp_dir();
        let path = dir.join("append.log");
        let _ = std::fs::remove_file(&path);
        {
            let mut f = open_regular_for_append(&path).unwrap();
            f.write_all(b"line1\n").unwrap();
        }
        {
            let mut f = open_regular_for_append(&path).unwrap();
            f.write_all(b"line2\n").unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn append_rejects_symlinked_final_component() {
        let dir = tmp_dir();
        let real = dir.join("real-target.log");
        std::fs::write(&real, b"pre\n").unwrap();
        let link = dir.join("link-target.log");
        let _ = std::fs::remove_file(&link);
        symlink(&real, &link).unwrap();
        let err = open_regular_for_append(&link).unwrap_err();
        assert!(
            matches!(
                err,
                RestrictedOpenError::Rejected(_) | RestrictedOpenError::Unsupported
            ),
            "unexpected: {err:?}"
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"pre\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn follows_relative_symlinks_and_parent_components() {
        let dir = tmp_dir().join("relative-links");
        std::fs::create_dir_all(dir.join("real/subdir")).unwrap();
        std::fs::write(dir.join("real/attachment.txt"), b"attachment\n").unwrap();
        symlink("real/subdir", dir.join("directory-link")).unwrap();
        symlink("directory-link/../attachment.txt", dir.join("file-link")).unwrap();
        assert_eq!(read_all(&dir.join("./file-link")), b"attachment\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn creates_appends_and_truncates_through_directory_symlinks() {
        let dir = tmp_dir().join("output-directory-links");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        symlink("real", dir.join("link")).unwrap();
        let path = dir.join("link/output.log");
        open_regular_for_append(&path)
            .unwrap()
            .write_all(b"first\n")
            .unwrap();
        open_regular_for_append(&path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nsecond\n");
        open_regular_for_create(&path)
            .unwrap()
            .write_all(b"replacement\n")
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement\n");

        let alias = dir.join("output-alias");
        symlink("real/output.log", &alias).unwrap();
        assert!(open_regular_for_create(&alias).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn refuses_symlink_loops_and_non_directory_components() {
        let dir = tmp_dir().join("invalid-paths");
        std::fs::create_dir_all(&dir).unwrap();
        symlink("loop", dir.join("loop")).unwrap();
        assert!(open_regular_for_read(&dir.join("loop")).is_err());
        std::fs::write(dir.join("file"), b"ordinary\n").unwrap();
        for suffix in ["file/", "file/.", "file/../file"] {
            let err = open_regular_for_read(&dir.join(suffix)).unwrap_err();
            assert!(
                matches!(err, RestrictedOpenError::Io(ref e)
                    if e.raw_os_error() == Some(libc::ENOTDIR)),
                "{suffix}: {err:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_devices_and_symlinked_pseudo_filesystems() {
        for path in ["/dev/null", "/dev/zero", "/sys/kernel/uevent_seqnum"] {
            assert!(matches!(
                open_regular_for_read(std::path::Path::new(path)),
                Err(RestrictedOpenError::Rejected(_))
            ));
        }
        let dir = tmp_dir().join("pseudo-filesystem-links");
        std::fs::create_dir_all(&dir).unwrap();
        symlink("/proc", dir.join("proc-alias")).unwrap();
        symlink("/dev/fd", dir.join("fd-alias")).unwrap();
        let ordinary = dir.join("ordinary");
        std::fs::write(&ordinary, b"ordinary\n").unwrap();
        let held = std::fs::File::open(&ordinary).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&held);
        for path in [
            dir.join("proc-alias/self/maps"),
            dir.join(format!("fd-alias/{fd}")),
        ] {
            assert!(matches!(
                open_regular_for_read(&path),
                Err(RestrictedOpenError::Rejected(_))
            ));
        }
    }
}
