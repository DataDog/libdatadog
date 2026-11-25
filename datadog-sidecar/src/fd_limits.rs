// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The open files limit of the sidecar, and what fills it.

use std::fmt;
#[cfg(unix)]
use std::io;
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicI32, Ordering};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
use tracing::error;
#[cfg(target_os = "linux")]
use tracing::{info, warn};

/// Below this soft limit a sidecar shared by a few hundred workers can run out of descriptors:
/// 1024 did, with 160 PHP-FPM workers and a slow agent.
#[cfg(target_os = "linux")]
const LOW_SOFT_LIMIT: libc::rlim_t = 4096;

/// The highest soft limit raised to: headroom, so that a descriptor leak still hits the limit and
/// hands the clients over to a fresh sidecar.
#[cfg(target_os = "linux")]
const SOFT_LIMIT_CAP: libc::rlim_t = 100_000;

// Otherwise every capped sidecar would warn of a low limit.
#[cfg(target_os = "linux")]
const _: () = assert!(LOW_SOFT_LIMIT <= SOFT_LIMIT_CAP);

/// The limits before the raise: a PHP-spawned sidecar logs nothing at startup, so the exhaustion
/// reports repeat them.
#[cfg(unix)]
static STARTUP_LIMITS: OnceLock<(libc::rlim_t, libc::rlim_t)> = OnceLock::new();

#[cfg(unix)]
fn open_files_limit() -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes `limit`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        Ok(limit)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn display(limit: libc::rlim_t) -> String {
    if limit == libc::RLIM_INFINITY {
        "unlimited".to_owned()
    } else {
        limit.to_string()
    }
}

/// The current limits, and the startup ones if they differ, as logged.
#[cfg(unix)]
pub(crate) fn describe_limits() -> String {
    match open_files_limit() {
        Ok(limit) => {
            let startup = match STARTUP_LIMITS.get() {
                Some(&(soft, hard)) if (soft, hard) != (limit.rlim_cur, limit.rlim_max) => {
                    format!(
                        " (soft={} hard={} at startup)",
                        display(soft),
                        display(hard)
                    )
                }
                _ => String::new(),
            };
            format!(
                "RLIMIT_NOFILE soft={} hard={}{startup}",
                display(limit.rlim_cur),
                display(limit.rlim_max)
            )
        }
        Err(e) => format!("RLIMIT_NOFILE unavailable ({e})"),
    }
}

#[cfg(not(unix))]
fn describe_limits() -> String {
    "RLIMIT_NOFILE unavailable".to_owned()
}

/// The soft limit to raise to, if any: the hard limit, at most [`SOFT_LIMIT_CAP`], and never a
/// lower one. An unlimited hard limit is above the cap.
#[cfg(target_os = "linux")]
fn raised_soft_limit(limit: &libc::rlimit) -> Option<libc::rlim_t> {
    let target = limit.rlim_max.min(SOFT_LIMIT_CAP);
    (limit.rlim_cur < target).then_some(target)
}

/// The sidecar serves every client, yet inherits the soft limit of the one that spawned it,
/// often 1024: raised as far as [`raised_soft_limit`] allows.
#[cfg(target_os = "linux")]
pub(crate) fn raise_open_files_limit() {
    let mut limit = match open_files_limit() {
        Ok(limit) => limit,
        Err(e) => {
            warn!("Could not read RLIMIT_NOFILE: {e}");
            return;
        }
    };
    _ = STARTUP_LIMITS.set((limit.rlim_cur, limit.rlim_max));
    info!("At startup, {}", describe_limits());
    if let Some(target) = raised_soft_limit(&limit) {
        let soft = limit.rlim_cur;
        limit.rlim_cur = target;
        // SAFETY: setrlimit only reads `limit`.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0 {
            info!(
                "Raised the RLIMIT_NOFILE soft limit from {soft} to {target}, capped at \
                 {SOFT_LIMIT_CAP}"
            );
        } else {
            warn!(
                "Could not raise the RLIMIT_NOFILE soft limit from {soft} to {target}: {}",
                io::Error::last_os_error()
            );
            limit.rlim_cur = soft;
        }
    } else {
        info!(
            "Left the RLIMIT_NOFILE soft limit at {}: already the hard limit, or at least \
             {SOFT_LIMIT_CAP}",
            display(limit.rlim_cur)
        );
    }
    if limit.rlim_cur < LOW_SOFT_LIMIT {
        warn!(
            "The RLIMIT_NOFILE soft limit is {}, below {LOW_SOFT_LIMIT}: the sidecar may run out \
             of descriptors with many clients. Raise the hard limit of the service that starts \
             PHP, e.g. systemd LimitNOFILE",
            display(limit.rlim_cur)
        );
    }
}

/// Logs `cause` at error level, with the limits and what fills the descriptor table.
pub(crate) fn report_exhaustion(cause: fmt::Arguments) {
    error!("{cause}; {}; {}", describe_descriptors(), describe_limits());
}

/// Kept open from startup: once the descriptor table is full, opening /proc/self/fd fails too.
#[cfg(target_os = "linux")]
static FD_DIRECTORY: Mutex<Option<FdDirectory>> = Mutex::new(None);

/// Raw descriptor of `FD_DIRECTORY`, or -1 when closed. Lets a forked child close an inherited
/// handle without blocking if another thread held `FD_DIRECTORY` at fork time.
#[cfg(target_os = "linux")]
static FD_DIRECTORY_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(target_os = "linux")]
struct FdDirectory {
    dir: std::ptr::NonNull<libc::DIR>,
    /// A forked child inherits the stream, which lists its parent's descriptors.
    pid: u32,
}

// SAFETY: the stream is only ever used under the mutex.
#[cfg(target_os = "linux")]
unsafe impl Send for FdDirectory {}

#[cfg(target_os = "linux")]
impl Drop for FdDirectory {
    fn drop(&mut self) {
        // SAFETY: the stream is live and owned by this value.
        let fd = unsafe { libc::dirfd(self.dir.as_ptr()) };
        _ = FD_DIRECTORY_FD.compare_exchange(fd, -1, Ordering::Relaxed, Ordering::Relaxed);
        unsafe { libc::closedir(self.dir.as_ptr()) };
    }
}

/// Opens what the diagnostics need while descriptors are still available.
pub(crate) fn prepare_diagnostics() {
    #[cfg(target_os = "linux")]
    if let Ok(mut directory) = FD_DIRECTORY.lock() {
        let pid = std::process::id();
        if directory
            .as_ref()
            .is_none_or(|directory| directory.pid != pid)
        {
            directory.take();
            // SAFETY: the path is a valid, NUL-terminated string.
            let dir = unsafe { libc::opendir(c"/proc/self/fd".as_ptr()) };
            if let Some(dir) = std::ptr::NonNull::new(dir) {
                // SAFETY: `dir` was just returned by `opendir`.
                FD_DIRECTORY_FD.store(unsafe { libc::dirfd(dir.as_ptr()) }, Ordering::Relaxed);
                *directory = Some(FdDirectory { dir, pid });
            }
        }
    }
}

/// Closes the pre-opened diagnostics directory handle (e.g. on orderly in-process shutdown).
#[cfg(unix)]
pub(crate) fn close_diagnostics() {
    #[cfg(target_os = "linux")]
    if let Ok(mut directory) = FD_DIRECTORY.lock() {
        directory.take();
    }
}

/// Closes any `/proc/self/fd` handle inherited across `fork()`.
///
/// # Safety
/// Call in the child after `fork()`, before starting threads.
#[cfg(unix)]
pub(crate) unsafe fn clear_inherited_state() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(mut directory) = FD_DIRECTORY.try_lock() {
            directory.take();
        } else {
            let fd = FD_DIRECTORY_FD.swap(-1, Ordering::Relaxed);
            if fd >= 0 {
                // SAFETY: inherited from the parent's `FD_DIRECTORY`; closing it only drops this
                // child's reference.
                unsafe { libc::close(fd) };
            }
        }
    }
}

/// One readlink per descriptor: the table can hold [`SOFT_LIMIT_CAP`] of them, more if the
/// inherited limit was higher.
#[cfg(target_os = "linux")]
const SCAN_BUDGET: Duration = Duration::from_millis(100);

/// Open descriptors by kind, counted without opening any.
#[cfg(target_os = "linux")]
fn describe_descriptors() -> String {
    let Ok(mut directory) = FD_DIRECTORY.lock() else {
        return "open_fds=unavailable".to_owned();
    };
    let Some(directory) = directory.as_mut() else {
        return "open_fds=unavailable".to_owned();
    };
    let dir = directory.dir.as_ptr();
    // SAFETY: the stream is live and exclusively borrowed for the whole iteration.
    let own = unsafe { libc::dirfd(dir) };
    unsafe { libc::rewinddir(dir) };
    let started = Instant::now();
    let mut complete = true;
    let (mut memfd, mut socket, mut pipe, mut anon_inode, mut other) = (0, 0, 0, 0, 0);
    loop {
        if started.elapsed() >= SCAN_BUDGET {
            complete = false;
            break;
        }
        // SAFETY: as above; the entry is used before the next readdir.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Some(fd) = name
            .to_str()
            .ok()
            .and_then(|name| name.parse::<libc::c_int>().ok())
        else {
            continue;
        };
        if fd == own {
            continue;
        }
        // readlink needs no descriptor, even with none left.
        let target = std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap_or_default();
        match target.as_os_str().as_encoded_bytes() {
            t if t.starts_with(b"/memfd:") => memfd += 1,
            t if t.starts_with(b"socket:") => socket += 1,
            t if t.starts_with(b"pipe:") => pipe += 1,
            t if t.starts_with(b"anon_inode:") => anon_inode += 1,
            _ => other += 1,
        }
    }
    let total = memfd + socket + pipe + anon_inode + other;
    let (at_least, cut) = if complete {
        ("", "")
    } else {
        (">", ", scan cut short")
    };
    format!(
        "open_fds{at_least}={total} (memfd={memfd} socket={socket} pipe={pipe} \
         anon_inode={anon_inode} other={other}{cut})"
    )
}

#[cfg(not(target_os = "linux"))]
fn describe_descriptors() -> String {
    "open_fds=unavailable".to_owned()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_soft_limit_is_raised_up_to_the_hard_limit_and_the_cap_never_lowered() {
        let raised = |soft, hard| {
            raised_soft_limit(&libc::rlimit {
                rlim_cur: soft,
                rlim_max: hard,
            })
        };
        assert_eq!(raised(256, 1_048_576), Some(100_000));
        assert_eq!(raised(256, 4096), Some(4096));
        assert_eq!(raised(200_000, 1_048_576), None);
        // Not settable for RLIMIT_NOFILE on Linux, which caps it at fs.nr_open.
        assert_eq!(raised(256, libc::RLIM_INFINITY), Some(100_000));
        assert_eq!(raised(4096, 4096), None);
        assert_eq!(raised(libc::RLIM_INFINITY, libc::RLIM_INFINITY), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn close_and_clear_release_the_fd_directory_handle() {
        const TEST: &str = "fd_limits::tests::close_and_clear_release_the_fd_directory_handle";
        if crate::test_util::is_rerun(TEST) {
            prepare_diagnostics();
            let opened = FD_DIRECTORY_FD.load(Ordering::Relaxed) >= 0;
            close_diagnostics();
            let closed = FD_DIRECTORY_FD.load(Ordering::Relaxed) == -1;
            prepare_diagnostics();
            let reopened = FD_DIRECTORY_FD.load(Ordering::Relaxed) >= 0;
            // SAFETY: single-threaded child process.
            unsafe { clear_inherited_state() };
            let cleared = FD_DIRECTORY_FD.load(Ordering::Relaxed) == -1;
            // SAFETY: exits directly with pass/fail status.
            unsafe {
                libc::_exit(if opened && closed && reopened && cleared {
                    0
                } else {
                    1
                })
            }
        }
        let status = crate::test_util::rerun(TEST).status().unwrap();
        assert_eq!(status.code(), Some(0));
    }
}
