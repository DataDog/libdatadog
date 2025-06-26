// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::timeout::TimeoutManager;
use ddcommon::unix_utils::{reap_child_non_blocking, wait_for_pollhup};
use nix::unistd::Pid;
use std::os::unix::io::RawFd;

pub(crate) struct ProcessHandle {
    pub uds_fd: RawFd,
    pub pid: Option<i32>,
}

impl ProcessHandle {
    pub fn new(uds_fd: RawFd, pid: Option<i32>) -> Self {
        Self { uds_fd, pid }
    }

    pub fn finish(&self, timeout_manager: &TimeoutManager) {
        let result = wait_for_pollhup(self.uds_fd, timeout_manager);
        debug_assert_eq!(result, Ok(true), "wait_for_pollhup failed: {result:?}");

        if let Some(pid) = self.pid {
            // If we have less than the minimum amount of time, give ourselves a few scheduler
            // slices worth of headroom to help guarantee that we don't leak a zombie process.
            let kill_result = unsafe { libc::kill(pid, libc::SIGKILL) };
            debug_assert_eq!(kill_result, 0, "kill failed with result: {}", kill_result);

            // `self` is actually a handle to a child process and `self.pid` is the child process's
            // pid.
            let child_pid = Pid::from_raw(pid);
            let result = reap_child_non_blocking(child_pid, timeout_manager);
            debug_assert_eq!(
                result,
                Ok(true),
                "reap_child_non_blocking failed: {result:?}"
            );
        }
    }
}
