// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::shared::constants::DD_CRASHTRACK_MINIMUM_REAP_TIME_MS;
use ddcommon::unix_utils::{reap_child_non_blocking, wait_for_pollhup};
use nix::unistd::Pid;
use std::os::unix::io::RawFd;
use std::time::Instant;

pub(crate) struct ProcessHandle {
    pub uds_fd: RawFd,
    pub pid: i32,
    pub oneshot: bool,
}

impl ProcessHandle {
    pub fn new(uds_fd: RawFd, pid: i32, oneshot: bool) -> Self {
        Self {
            uds_fd,
            pid,
            oneshot,
        }
    }

    pub fn finish(&self, start_time: Instant, timeout_ms: u32) {
        let pollhup_allowed_ms = timeout_ms
            .saturating_sub(start_time.elapsed().as_millis() as u32)
            .min(i32::MAX as u32) as i32;
        let _ = wait_for_pollhup(self.uds_fd, pollhup_allowed_ms);

        if self.oneshot {
            // If we have less than the minimum amount of time, give ourselves a few scheduler
            // slices worth of headroom to help guarantee that we don't leak a zombie process.
            let _ = unsafe { libc::kill(self.pid, libc::SIGKILL) };

            // `self` is actually a handle to a child process and `self.pid` is the child process's
            // pid.
            let child_pid = Pid::from_raw(self.pid);
            let reaping_allowed_ms = std::cmp::min(
                timeout_ms.saturating_sub(start_time.elapsed().as_millis() as u32),
                DD_CRASHTRACK_MINIMUM_REAP_TIME_MS,
            );
            let _ = reap_child_non_blocking(child_pid, reaping_allowed_ms);
        }
    }
}
