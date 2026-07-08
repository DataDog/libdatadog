// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU32, Ordering};

use super::sys;

/// Declares a `#[repr(transparent)]` u32 bitset newtype with the common flag
/// operations shared by [`Capabilities`] and [`Degradations`].
macro_rules! bitset_u32 {
    ($name:ident) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $name(u32);

        impl $name {
            pub const fn empty() -> Self {
                Self(0)
            }

            pub const fn from_bits(bits: u32) -> Self {
                Self(bits)
            }

            pub const fn bits(self) -> u32 {
                self.0
            }

            pub const fn contains(self, flag: Self) -> bool {
                self.0 & flag.0 != 0
            }

            fn insert(&mut self, flag: Self) {
                self.0 |= flag.0;
            }
        }
    };
}

bitset_u32!(Capabilities);
bitset_u32!(Degradations);

impl Degradations {
    pub const fn with(self, flag: Self) -> Self {
        Self(self.0 | flag.0)
    }
}

pub const RECEIVER_OK: Capabilities = Capabilities::from_bits(1 << 0);
pub const PROC_VM_READV: Capabilities = Capabilities::from_bits(1 << 1);
pub const FORK_OK: Capabilities = Capabilities::from_bits(1 << 2);
pub const DEV_NULL: Capabilities = Capabilities::from_bits(1 << 3);
pub const PIPE_OK: Capabilities = Capabilities::from_bits(1 << 4);
pub const REPORT_FD_OK: Capabilities = Capabilities::from_bits(1 << 5);

pub const DEGRADED_MISSING_RECEIVER: Degradations = Degradations::from_bits(1 << 0);
pub const DEGRADED_NO_PROC_VM_READV: Degradations = Degradations::from_bits(1 << 1);
pub const DEGRADED_NO_FORK: Degradations = Degradations::from_bits(1 << 2);
pub const DEGRADED_NO_DEV_NULL: Degradations = Degradations::from_bits(1 << 3);
pub const DEGRADED_NO_PIPE: Degradations = Degradations::from_bits(1 << 4);
pub const DEGRADED_PIPE_FAILED: Degradations = Degradations::from_bits(1 << 5);
pub const DEGRADED_FORK_FAILED: Degradations = Degradations::from_bits(1 << 6);
pub const DEGRADED_RECEIVER_UNAVAILABLE: Degradations = Degradations::from_bits(1 << 7);
pub const DEGRADED_REPORT_TO_FD: Degradations = Degradations::from_bits(1 << 8);
pub const DEGRADED_TRUNCATED: Degradations = Degradations::from_bits(1 << 9);
pub const DEGRADED_METADATA_TRUNCATED: Degradations = Degradations::from_bits(1 << 10);
pub const DEGRADED_APP_HANDLER_PRESENT: Degradations = Degradations::from_bits(1 << 11);
pub const DEGRADED_ALT_STACK_GUARD_UNAVAILABLE: Degradations = Degradations::from_bits(1 << 12);

pub const DEGRADATION_REASONS: &[(Degradations, &str)] = &[
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
    let mut caps = Capabilities::empty();
    let mut degraded = Degradations::empty();

    if sys::access_executable(receiver_path.as_ptr()) {
        caps.insert(RECEIVER_OK);
    } else {
        degraded.insert(DEGRADED_MISSING_RECEIVER);
    }

    if probe_process_vm_readv() && (!probe_seccomp || probe_process_vm_readv_in_child()) {
        caps.insert(PROC_VM_READV);
    } else {
        degraded.insert(DEGRADED_NO_PROC_VM_READV);
    }

    if sys::fork_supported() {
        caps.insert(FORK_OK);
    } else {
        degraded.insert(DEGRADED_NO_FORK);
    }

    let devnull = sys::open_readwrite(c"/dev/null".as_ptr().cast());
    if devnull >= 0 {
        caps.insert(DEV_NULL);
        sys::close(devnull);
    } else {
        degraded.insert(DEGRADED_NO_DEV_NULL);
    }

    if let Some(pipe) = sys::pipe() {
        caps.insert(PIPE_OK);
        sys::close(pipe.read);
        sys::close(pipe.write);
    } else {
        degraded.insert(DEGRADED_NO_PIPE);
    }

    if sys::fd_valid(report_fd) {
        caps.insert(REPORT_FD_OK);
    }

    CAPABILITIES.store(caps.bits(), Ordering::Release);
    DEGRADATIONS.store(degraded.bits(), Ordering::Release);
}

pub fn get() -> Capabilities {
    Capabilities::from_bits(CAPABILITIES.load(Ordering::Acquire))
}

pub fn has(capability: Capabilities) -> bool {
    get().contains(capability)
}

pub fn degradations() -> Degradations {
    Degradations::from_bits(DEGRADATIONS.load(Ordering::Acquire))
}

pub fn note_degraded(reason: Degradations) {
    DEGRADATIONS.fetch_or(reason.bits(), Ordering::AcqRel);
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

    match sys::reap_child(child as i32, 100, 10, 10) {
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

        assert!(!get().contains(RECEIVER_OK));
        assert!(degradations().contains(DEGRADED_MISSING_RECEIVER));
        assert!(!get().contains(REPORT_FD_OK));
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

        assert!(get().contains(REPORT_FD_OK));
    }
}
