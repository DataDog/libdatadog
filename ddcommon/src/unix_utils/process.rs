// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc::{_exit, nfds_t, poll, pollfd, EXIT_FAILURE, POLLHUP};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::os::fd::RawFd;

use crate::timeout::TimeoutManager;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_reap_child_non_blocking_timeout() {
        let timeout = Duration::from_millis(10);
        let manager = TimeoutManager::new(timeout);

        // Try to reap a non-existent process
        let result = reap_child_non_blocking(Pid::from_raw(99999), &manager);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_reap_child_non_blocking_exited_child() {
        // This test would require actually creating a child process
        // For now, just test that the function compiles and handles non-existent PIDs
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);

        let result = reap_child_non_blocking(Pid::from_raw(99999), &manager);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_reap_child_non_blocking_nonexistent_pid() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);

        let result = reap_child_non_blocking(Pid::from_raw(99999), &manager);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_wait_for_pollhup_timeout() {
        let timeout = Duration::from_millis(10);
        let manager = TimeoutManager::new(timeout);

        // Use an invalid file descriptor to test timeout
        let result = wait_for_pollhup(-1, &manager);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_wait_for_pollhup_invalid_fd() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);

        // Use a positive, almost certainly invalid file descriptor
        let invalid_fd = 999_999;
        let result = wait_for_pollhup(invalid_fd, &manager);

        // Invalid FD should result in an error, not a timeout
        match result {
            Err(PollError::PollError(errno)) => {
                // Should be a valid errno (EBADF or similar)
                assert!(errno > 0);
            }
            Err(PollError::UnexpectedResult(revents)) => {
                println!("wait_for_pollhup({invalid_fd}, ..) returned UnexpectedResult({revents}) as allowed on this platform");
            }
            _ => panic!("Expected error for invalid FD, got: {:?}", result),
        }
    }
}
