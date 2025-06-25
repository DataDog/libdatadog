// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use anyhow::Context;
use libc::{_exit, execve, nfds_t, poll, pollfd, EXIT_FAILURE, POLLHUP};
use nix::errno::Errno;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::os::fd::IntoRawFd;
use std::time::{Duration, Instant};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::fd::RawFd,
};

#[cfg(target_os = "linux")]
use std::io::{self, BufRead, BufReader};

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReapError {
    #[error("Timeout waiting for child process to exit")]
    Timeout,
    #[error("Error waiting for child process to exit: {0}")]
    WaitError(#[from] nix::Error),
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum PollError {
    #[error("Poll failed with errno: {0}")]
    PollError(i32),
    #[error("Poll returned unexpected result: revents = {0}")]
    UnexpectedResult(i16),
}

pub struct TimeoutManager {
    start_time: Instant,
    timeout: Duration,
}

impl TimeoutManager {
    // 4ms per sched slice, give ~4x10 slices for safety
    const MINIMUM_REAP_TIME: Duration = Duration::from_millis(160);
    pub fn new(timeout: Duration) -> Self {
        Self {
            start_time: Instant::now(),
            timeout,
        }
    }

    pub fn remaining(&self) -> Duration {
        // If elapsed > timeout, remaining will be 0
        let elapsed = self.start_time.elapsed();
        if elapsed >= self.timeout {
            Self::MINIMUM_REAP_TIME
        } else {
            (self.timeout - elapsed).max(Self::MINIMUM_REAP_TIME)
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl std::fmt::Debug for TimeoutManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeoutManager")
            .field("start_time", &self.start_time)
            .field("elapsed", &self.elapsed())
            .field("timeout", &self.timeout)
            .field("remaining", &self.remaining())
            .finish()
    }
}

// The args_cstrings and env_vars_strings fields are just storage.  Even though they're
// unreferenced, they're a necessary part of the struct.
#[allow(dead_code)]
pub struct PreparedExecve {
    binary_path: CString,
    args_cstrings: Vec<CString>,
    args_ptrs: Vec<*const libc::c_char>,
    env_vars_cstrings: Vec<CString>,
    env_vars_ptrs: Vec<*const libc::c_char>,
}

impl PreparedExecve {
    pub fn new(binary_path: &str, args: &[String], env: &[(String, String)]) -> Self {
        // Allocate and store binary path
        #[allow(clippy::expect_used)]
        let binary_path =
            CString::new(binary_path).expect("Failed to convert binary path to CString");

        // Allocate and store arguments
        #[allow(clippy::expect_used)]
        let args_cstrings: Vec<CString> = args
            .iter()
            .map(|s| CString::new(s.as_str()).expect("Failed to convert argument to CString"))
            .collect();
        let args_ptrs: Vec<*const libc::c_char> = args_cstrings
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        // Allocate and store environment variables
        let env_vars_cstrings: Vec<CString> = env
            .iter()
            .map(|(key, value)| {
                let env_str = format!("{key}={value}");
                #[allow(clippy::expect_used)]
                CString::new(env_str).expect("Failed to convert environment variable to CString")
            })
            .collect();
        let env_vars_ptrs: Vec<*const libc::c_char> = env_vars_cstrings
            .iter()
            .map(|env| env.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        Self {
            binary_path,
            args_cstrings,
            args_ptrs,
            env_vars_cstrings,
            env_vars_ptrs,
        }
    }

    /// Calls `execve` on the prepared arguments.
    pub fn exec(&self) -> Result<(), Errno> {
        // Safety: the only way to make one of these is through `new`, which ensures that everything
        // is well-formed.
        unsafe {
            if execve(
                self.binary_path.as_ptr(),
                self.args_ptrs.as_ptr(),
                self.env_vars_ptrs.as_ptr(),
            ) == -1
            {
                Err(Errno::last())
            } else {
                Ok(())
            }
        }
    }
}

/// Opens a file for writing (in append mode) or opens /dev/null
/// * If the filename is provided, it will try to open (creating if needed) the specified file.
///   Failure to do so is an error.
/// * If the filename is not provided, it will open /dev/null Some systems can fail to provide
///   `/dev/null` (e.g., chroot jails), so this failure is also an error.
/// * Using Stdio::null() is more direct, but it will cause a panic in environments where /dev/null
///   is not available.
pub fn open_file_or_quiet(filename: Option<&str>) -> anyhow::Result<RawFd> {
    let file = filename.map_or_else(
        || File::open("/dev/null").context("Failed to open /dev/null"),
        |f| {
            OpenOptions::new()
                .append(true)
                .create(true)
                .open(f)
                .with_context(|| format!("Failed to open or create file: {f}"))
        },
    )?;
    Ok(file.into_raw_fd())
}

/// Non-blocking child reaper
/// * If the child process has exited, return true
/// * If the child process cannot be found, return false
/// * If the child is still alive, or some other error occurs, return an error Either way, after
///   this returns, you probably don't have to do anything else.
// Note: some resources indicate it is unsafe to call `waitpid` from a signal handler, especially
//       on macos, where the OS will terminate an offending process.  This appears to be untrue
//       and `waitpid()` is characterized as async-signal safe by POSIX.
pub fn reap_child_non_blocking(
    pid: Pid,
    timeout_manager: &TimeoutManager,
) -> Result<bool, ReapError> {
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if timeout_manager.elapsed() > timeout_manager.timeout() {
                    return Err(ReapError::Timeout);
                }
                // TODO, this is currently a busy loop.  Consider sleeping for a short time.
            }
            Ok(_status) => return Ok(true),
            Err(nix::Error::ECHILD) => {
                // Non-availability of the specified process is weird, since we should have
                // exclusive access to reaping its exit, but at the very least means there is
                // nothing further for us to do.
                return Ok(false);
            }
            Err(e) => return Err(ReapError::WaitError(e)),
        }
    }
}

/// Kills the program without raising an abort or calling at_exit
pub fn terminate() -> ! {
    // Safety: No preconditions
    unsafe { _exit(EXIT_FAILURE) }
}

/// true if successful wait, false if timeout occurred.
pub fn wait_for_pollhup(
    target_fd: RawFd,
    timeout_manager: &TimeoutManager,
) -> Result<bool, PollError> {
    let mut poll_fds = [pollfd {
        fd: target_fd,
        events: POLLHUP,
        revents: 0,
    }];

    loop {
        let timeout_ms = timeout_manager.remaining().as_millis() as i32;
        let poll_result =
            unsafe { poll(poll_fds.as_mut_ptr(), poll_fds.len() as nfds_t, timeout_ms) };
        match poll_result {
            -1 => {
                match nix::Error::last_raw() {
                    libc::EAGAIN | libc::EINTR => {
                        // Retry on EAGAIN or EINTR
                        continue;
                    }
                    errno => return Err(PollError::PollError(errno)),
                }
            }
            0 => return Ok(false), // Timeout occurred
            _ => {
                let revents = poll_fds[0].revents;
                if revents & POLLHUP != 0 {
                    return Ok(true); // POLLHUP detected
                } else {
                    return Err(PollError::UnexpectedResult(revents));
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn alt_fork() -> i32 {
    // There is a lower-level `__fork()` function in macOS, and we can call it from Rust, but the
    // runtime is much stricter about which operations (e.g., no malloc) are allowed in the child.
    // This somewhat defeats the purpose, so macOS for now will just have to live with atfork
    // handlers.
    unsafe { libc::fork() }
}

#[cfg(target_os = "linux")]
fn is_being_traced() -> io::Result<bool> {
    // Check to see whether we are being traced.  This will fail on systems where procfs is
    // unavailable, but presumably in those systems `ptrace()` is also unavailable.
    // The caller is free to treat a failure as a false.
    let file = File::open("/proc/self/status")?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if line.starts_with("TracerPid:") {
            let tracer_pid = line.split_whitespace().nth(1).unwrap_or("0");
            return Ok(tracer_pid != "0");
        }
    }

    Ok(false)
}

#[cfg(target_os = "linux")]
pub fn alt_fork() -> libc::pid_t {
    use libc::{
        c_ulong, c_void, pid_t, syscall, SYS_clone, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID,
        CLONE_PTRACE, SIGCHLD,
    };

    let mut _ptid: pid_t = 0;
    let mut _ctid: pid_t = 0;

    // Check whether we're traced before we fork.
    let being_traced = is_being_traced().unwrap_or(false);
    let extra_flags = if being_traced { CLONE_PTRACE } else { 0 };

    // Use the direct syscall interface into `clone()`.  This should replicate the parameters used
    // for glibc `fork()`, except of course without calling the atfork handlers.
    // One question is whether we're using the right set of flags.  For instance, does suppressing
    // `SIGCHLD` here make it easier for us to handle some conditions in the parent process?
    let res = unsafe {
        syscall(
            SYS_clone,
            (CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID | SIGCHLD | extra_flags) as c_ulong,
            std::ptr::null_mut::<c_void>(),
            &mut _ptid as *mut pid_t,
            &mut _ctid as *mut pid_t,
            0 as c_ulong,
        )
    };

    // The max value of a PID is configurable, but within an i32, so the failover
    if res > pid_t::MAX as i64 {
        pid_t::MAX
    } else if res < pid_t::MIN as i64 {
        pid_t::MIN
    } else {
        res as pid_t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_manager_new() {
        let timeout = Duration::from_secs(5);
        let manager = TimeoutManager::new(timeout);
        
        assert_eq!(manager.timeout(), timeout);
        assert!(manager.elapsed() < Duration::from_millis(100)); // Should be very small
        assert!(manager.remaining() >= TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_remaining() {
        let timeout = Duration::from_millis(100);
        let manager = TimeoutManager::new(timeout);
        
        // Initially, remaining should be close to timeout but at least MINIMUM_REAP_TIME
        let remaining = manager.remaining();
        assert!(remaining >= TimeoutManager::MINIMUM_REAP_TIME);
        // Note: remaining might be greater than timeout due to MINIMUM_REAP_TIME
        
        // After sleeping, remaining should decrease (but still respect MINIMUM_REAP_TIME)
        std::thread::sleep(Duration::from_millis(10));
        let remaining_after_sleep = manager.remaining();
        assert!(remaining_after_sleep >= TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_elapsed() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);
        
        // Initially elapsed should be very small
        assert!(manager.elapsed() < Duration::from_millis(100));
        
        // After sleeping, elapsed should increase
        std::thread::sleep(Duration::from_millis(10));
        let elapsed = manager.elapsed();
        assert!(elapsed >= Duration::from_millis(10));
        assert!(elapsed < Duration::from_millis(100)); // Should be reasonable
    }

    #[test]
    fn test_timeout_manager_minimum_reap_time() {
        let timeout = Duration::from_millis(50); // Less than MINIMUM_REAP_TIME
        let manager = TimeoutManager::new(timeout);
        
        // Even with a small timeout, remaining should be at least MINIMUM_REAP_TIME
        assert_eq!(manager.remaining(), TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_debug() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);
        
        let debug_str = format!("{:?}", manager);
        
        // Debug output should contain the expected fields
        assert!(debug_str.contains("TimeoutManager"));
        assert!(debug_str.contains("start_time"));
        assert!(debug_str.contains("elapsed"));
        assert!(debug_str.contains("timeout"));
        assert!(debug_str.contains("remaining"));
    }

    #[test]
    fn test_timeout_manager_timeout_exceeded() {
        let timeout = Duration::from_millis(10);
        let manager = TimeoutManager::new(timeout);
        
        // Sleep longer than the timeout
        std::thread::sleep(Duration::from_millis(50));
        
        // Elapsed should be greater than timeout
        assert!(manager.elapsed() > timeout);
        
        // Remaining should still be at least MINIMUM_REAP_TIME (not overflow)
        let remaining = manager.remaining();
        assert_eq!(remaining, TimeoutManager::MINIMUM_REAP_TIME);
    }
}
