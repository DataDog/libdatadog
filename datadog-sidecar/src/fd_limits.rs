// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The open files limit of the sidecar, and what fills it.

use std::fmt;
#[cfg(unix)]
use std::io;
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tracing::error;
#[cfg(target_os = "linux")]
use tracing::{info, warn};

/// Below this soft limit a sidecar shared by a few hundred workers can run out of descriptors:
/// 1024 did, with 160 PHP-FPM workers and a slow agent.
#[cfg(target_os = "linux")]
const LOW_SOFT_LIMIT: libc::rlim_t = 4096;

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

/// The sidecar serves every client, yet inherits the soft limit of the one that spawned it,
/// often 1024. Only the hard limit is meant as a ceiling.
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
    if limit.rlim_cur < limit.rlim_max {
        let soft = limit.rlim_cur;
        limit.rlim_cur = limit.rlim_max;
        // SAFETY: setrlimit only reads `limit`.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0 {
            info!(
                "Raised the RLIMIT_NOFILE soft limit from {soft} to {}",
                display(limit.rlim_max)
            );
        } else {
            warn!(
                "Could not raise the RLIMIT_NOFILE soft limit from {soft} to {}: {}",
                display(limit.rlim_max),
                io::Error::last_os_error()
            );
            limit.rlim_cur = soft;
        }
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

/// At most one report per interval: a full descriptor table fails every accept, and every
/// message carrying a descriptor, at once.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);
static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);
static SUPPRESSED: AtomicUsize = AtomicUsize::new(0);

/// Logs `cause` at error level, with the limits and what fills the descriptor table.
pub(crate) fn report_exhaustion(cause: fmt::Arguments) {
    if let Ok(mut last) = LAST_REPORT.lock() {
        let now = Instant::now();
        if (*last).is_some_and(|last| now.duration_since(last) < REPORT_INTERVAL) {
            SUPPRESSED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        *last = Some(now);
    }
    let suppressed = match SUPPRESSED.swap(0, Ordering::Relaxed) {
        0 => String::new(),
        n => format!(" ({n} similar errors suppressed)"),
    };
    error!(
        "{cause}; {}; {}{suppressed}",
        describe_descriptors(),
        describe_limits()
    );
}

/// Kept open from startup: once the descriptor table is full, opening /proc/self/fd fails too.
#[cfg(target_os = "linux")]
static FD_DIRECTORY: Mutex<Option<FdDirectory>> = Mutex::new(None);

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
            // SAFETY: the path is a valid, NUL-terminated string.
            let dir = unsafe { libc::opendir(c"/proc/self/fd".as_ptr()) };
            *directory = std::ptr::NonNull::new(dir).map(|dir| FdDirectory { dir, pid });
        }
    }
}

/// One readlink per descriptor: with a raised limit the table can hold a million of them.
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
