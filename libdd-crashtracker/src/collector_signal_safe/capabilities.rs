// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU32, Ordering};

use super::sys;

pub const RECEIVER_OK: u32 = 1 << 0;
pub const PROC_VM_READV: u32 = 1 << 1;
pub const FORK_OK: u32 = 1 << 2;
pub const DEV_NULL: u32 = 1 << 3;
pub const PIPE_OK: u32 = 1 << 4;
pub const REPORT_FD_OK: u32 = 1 << 5;

pub const DEGRADED_MISSING_RECEIVER: u32 = 1 << 0;
pub const DEGRADED_NO_PROC_VM_READV: u32 = 1 << 1;
pub const DEGRADED_NO_FORK: u32 = 1 << 2;
pub const DEGRADED_NO_DEV_NULL: u32 = 1 << 3;
pub const DEGRADED_NO_PIPE: u32 = 1 << 4;
pub const DEGRADED_PIPE_FAILED: u32 = 1 << 5;
pub const DEGRADED_FORK_FAILED: u32 = 1 << 6;
pub const DEGRADED_RECEIVER_UNAVAILABLE: u32 = 1 << 7;
pub const DEGRADED_REPORT_TO_FD: u32 = 1 << 8;
pub const DEGRADED_TRUNCATED: u32 = 1 << 9;
pub const DEGRADED_METADATA_TRUNCATED: u32 = 1 << 10;
pub const DEGRADED_APP_HANDLER_PRESENT: u32 = 1 << 11;
pub const DEGRADED_ALT_STACK_GUARD_UNAVAILABLE: u32 = 1 << 12;

pub const DEGRADATION_REASONS: &[(u32, &str)] = &[
    (DEGRADED_MISSING_RECEIVER, "missing_receiver"),
    (DEGRADED_NO_PROC_VM_READV, "no_process_vm_readv"),
    (DEGRADED_NO_FORK, "no_fork"),
    (DEGRADED_NO_DEV_NULL, "no_dev_null"),
    (DEGRADED_NO_PIPE, "no_pipe"),
    (DEGRADED_PIPE_FAILED, "pipe_failed"),
    (DEGRADED_FORK_FAILED, "fork_failed"),
    (DEGRADED_RECEIVER_UNAVAILABLE, "receiver_unavailable"),
    (DEGRADED_REPORT_TO_FD, "report_to_fd"),
    (DEGRADED_TRUNCATED, "truncated"),
    (DEGRADED_METADATA_TRUNCATED, "metadata_truncated"),
    (DEGRADED_APP_HANDLER_PRESENT, "app_handler_present"),
    (
        DEGRADED_ALT_STACK_GUARD_UNAVAILABLE,
        "alt_stack_guard_unavailable",
    ),
];

static CAPABILITIES: AtomicU32 = AtomicU32::new(0);
static DEGRADATIONS: AtomicU32 = AtomicU32::new(0);

pub fn publish(receiver_path: &[u8], report_fd: i32, probe_seccomp: bool) {
    let mut caps = 0u32;
    let mut degraded = 0u32;

    if sys::access_executable(receiver_path.as_ptr()) {
        caps |= RECEIVER_OK;
    } else {
        degraded |= DEGRADED_MISSING_RECEIVER;
    }

    if probe_process_vm_readv() && (!probe_seccomp || probe_process_vm_readv_in_child()) {
        caps |= PROC_VM_READV;
    } else {
        degraded |= DEGRADED_NO_PROC_VM_READV;
    }

    if sys::fork_supported() {
        caps |= FORK_OK;
    } else {
        degraded |= DEGRADED_NO_FORK;
    }

    let devnull = sys::open_readwrite(c"/dev/null".as_ptr().cast());
    if devnull >= 0 {
        caps |= DEV_NULL;
        sys::close(devnull);
    } else {
        degraded |= DEGRADED_NO_DEV_NULL;
    }

    let mut fds = [0i32; 2];
    if sys::pipe(&mut fds) {
        caps |= PIPE_OK;
        sys::close(fds[0]);
        sys::close(fds[1]);
    } else {
        degraded |= DEGRADED_NO_PIPE;
    }

    if sys::fd_valid(report_fd) {
        caps |= REPORT_FD_OK;
    }

    CAPABILITIES.store(caps, Ordering::Release);
    DEGRADATIONS.store(degraded, Ordering::Release);
}

pub fn get() -> u32 {
    CAPABILITIES.load(Ordering::Acquire)
}

pub fn has(capability: u32) -> bool {
    get() & capability != 0
}

pub fn degradations() -> u32 {
    DEGRADATIONS.load(Ordering::Acquire)
}

pub fn note_degraded(reason: u32) {
    DEGRADATIONS.fetch_or(reason, Ordering::AcqRel);
}

fn probe_process_vm_readv() -> bool {
    let src = 0x5au8;
    let mut dst = [0u8; 1];
    sys::read_own_mem(sys::getpid(), (&src as *const u8) as usize, &mut dst) && dst[0] == src
}

fn probe_process_vm_readv_in_child() -> bool {
    if !sys::fork_supported() {
        return true;
    }

    let child = unsafe { sys::fork_raw() };
    if child == 0 {
        sys::exit_process(if probe_process_vm_readv() { 0 } else { 1 });
    }
    if child < 0 {
        return true;
    }

    match sys::reap_child(child as i32, 100, 10, true, 10) {
        sys::ChildReap::Reaped(status) => libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        sys::ChildReap::NoChild | sys::ChildReap::WaitFailed(_) | sys::ChildReap::TimedOut => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn publish_reports_missing_receiver_degradation() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        publish(b"/definitely/missing-signal-safe-receiver\0", -1, false);

        assert_eq!(get() & RECEIVER_OK, 0);
        assert_ne!(degradations() & DEGRADED_MISSING_RECEIVER, 0);
        assert_eq!(get() & REPORT_FD_OK, 0);
    }

    #[test]
    fn publish_marks_valid_report_fd() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");
        let file = tempfile::tempfile().expect("tempfile");

        publish(
            b"/definitely/missing-signal-safe-receiver\0",
            file.as_raw_fd(),
            false,
        );

        assert_ne!(get() & REPORT_FD_OK, 0);
    }
}
