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
pub fn reap_child_non_blocking(pid: Pid, timeout_ms: u32) -> anyhow::Result<bool> {
    let timeout = Duration::from_millis(timeout_ms.into());
    let start_time = Instant::now();

    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => anyhow::ensure!(
                start_time.elapsed() <= timeout,
                "Timeout waiting for child process to exit"
            ),
            Ok(_status) => return Ok(true),
            Err(nix::Error::ECHILD) => {
                // Non-availability of the specified process is weird, since we should have
                // exclusive access to reaping its exit, but at the very least means there is
                // nothing further for us to do.
                return Ok(true);
            }
            _ => anyhow::bail!("Error waiting for child process to exit"),
        }
    }
}

/// true if successful wait, false if timeout occurred.
pub fn wait_for_pollhup(target_fd: RawFd, timeout_ms: i32) -> anyhow::Result<bool> {
    let mut poll_fds = [pollfd {
        fd: target_fd,
        events: POLLHUP,
        revents: 0,
    }];

    match unsafe { poll(poll_fds.as_mut_ptr(), poll_fds.len() as nfds_t, timeout_ms) } {
        -1 => Err(anyhow::anyhow!(
            "poll failed with errno: {}",
            std::io::Error::last_os_error()
        )),
        0 => Ok(false), // Timeout occurred
        _ => {
            let revents = poll_fds[0].revents;
            anyhow::ensure!(
                revents & POLLHUP != 0,
                "poll returned unexpected result: revents = {revents}"
            );
            Ok(true) // POLLHUP detected
        }
    }
}

/// Kills the program without raising an abort or calling at_exit
pub fn terminate() -> ! {
    unsafe { _exit(EXIT_FAILURE) }
}
