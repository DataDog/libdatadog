// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::process_handle::ProcessHandle;
use super::receiver_manager::Receiver;
use ddcommon::timeout::TimeoutManager;

use super::emitters::emit_crashreport;
use crate::shared::configuration::CrashtrackerConfiguration;
use ddcommon::unix_utils::{alt_fork, terminate};
use libc::{siginfo_t, ucontext_t};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use std::os::unix::io::RawFd;
use std::os::unix::{io::FromRawFd, net::UnixStream};

pub(crate) struct Collector {
    pub handle: ProcessHandle,
}

impl Collector {
    pub(crate) fn spawn(
        receiver: &Receiver,
        config: &CrashtrackerConfiguration,
        config_str: &str,
        metadata_str: &str,
        sig_info: *const siginfo_t,
        ucontext: *const ucontext_t,
    ) -> anyhow::Result<Self> {
        // When we spawn the child, our pid becomes the ppid.
        // SAFETY: This function has no safety requirements.
        let pid = unsafe { libc::getpid() };

        match alt_fork() {
            0 => {
                // Child (does not exit from this function)
                run_collector_child(
                    config,
                    config_str,
                    metadata_str,
                    sig_info,
                    ucontext,
                    receiver.handle.uds_fd,
                    pid,
                );
            }
            pid if pid > 0 => Ok(Self {
                handle: ProcessHandle::new(receiver.handle.uds_fd, Some(pid)),
            }),
            _ => {
                // Error
                Err(anyhow::anyhow!("Failed to fork collector process"))
            }
        }
    }

    pub fn finish(self, timeout_manager: &TimeoutManager) {
        self.handle.finish(timeout_manager);
    }
}

pub(crate) fn run_collector_child(
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_str: &str,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
    uds_fd: RawFd,
    ppid: libc::pid_t,
) -> ! {
    // Close stdio
    let _ = unsafe { libc::close(0) };
    let _ = unsafe { libc::close(1) };
    let _ = unsafe { libc::close(2) };

    // Disable SIGPIPE
    let _ = unsafe {
        signal::sigaction(
            signal::SIGPIPE,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )
    };

    // Emit crashreport
    let mut unix_stream = unsafe { UnixStream::from_raw_fd(uds_fd) };

    let report = emit_crashreport(
        &mut unix_stream,
        config,
        config_str,
        metadata_str,
        sig_info,
        ucontext,
        ppid,
    );
    if let Err(e) = report {
        eprintln!("Failed to flush crash report: {e}");
        terminate();
    }

    // Exit normally
    unsafe { libc::_exit(0) };
}
