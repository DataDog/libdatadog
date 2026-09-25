// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::process_handle::ProcessHandle;
use super::receiver_manager::Receiver;
use libdd_common::timeout::TimeoutManager;
use std::time::Instant;

use super::emitters::{emit_crashreport, CrashKindData};
use crate::shared::configuration::CrashtrackerConfiguration;
use libdd_common::unix_utils::{alt_fork, terminate};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use std::os::unix::io::RawFd;
use std::os::unix::{io::FromRawFd, net::UnixStream};
use thiserror::Error;

pub(crate) struct Collector {
    pub handle: ProcessHandle,
}

#[derive(Debug, Error)]
pub enum CollectorSpawnError {
    #[error("Failed to fork collector process (error code: {0})")]
    ForkFailed(i32),
}

impl Collector {
    pub(crate) fn spawn(
        receiver: &Receiver,
        config: &CrashtrackerConfiguration,
        config_str: &str,
        metadata_str: &str,
        message: Option<&str>,
        crash: CrashKindData,
        threads_deadline: Instant,
    ) -> Result<Self, CollectorSpawnError> {
        // When we spawn the child, our pid becomes the ppid.
        // SAFETY: This function has no safety requirements.
        let pid = unsafe { libc::getpid() };

        // Get the current tid to identify thread info
        let tid = current_tid();

        // Thread collection needs the collector to ptrace us, and Yama permits that only once
        // we name it as our tracer - which we cannot do before the fork tells us its pid. The
        // child therefore blocks on this pipe until we have granted it.
        #[cfg(target_os = "linux")]
        let ptrace_gate = if config.collect_all_threads() {
            make_ptrace_gate()
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let ptrace_gate = None;

        let fork_result = alt_fork();
        match fork_result {
            0 => {
                // Child (does not exit from this function)
                run_collector_child(
                    config,
                    config_str,
                    metadata_str,
                    message,
                    crash,
                    receiver.handle.uds_fd,
                    pid,
                    tid,
                    ptrace_gate,
                    threads_deadline,
                );
            }
            pid if pid > 0 => {
                #[cfg(target_os = "linux")]
                if let Some((read_fd, write_fd)) = ptrace_gate {
                    // SAFETY: both calls are async-signal-safe. Closing the write end is what
                    // releases the child, so it must come after the grant.
                    unsafe {
                        libc::prctl(libc::PR_SET_PTRACER, pid as libc::c_ulong);
                        libc::close(read_fd);
                        libc::close(write_fd);
                    }
                }
                Ok(Self {
                    handle: ProcessHandle::new(receiver.handle.uds_fd, Some(pid)),
                })
            }
            code => {
                // Error
                Err(CollectorSpawnError::ForkFailed(code))
            }
        }
    }

    pub fn finish(self, timeout_manager: &TimeoutManager) {
        self.handle.finish(timeout_manager);
    }
}

/// A pipe the collector child blocks on until we have granted it ptrace permission.
///
/// `None` when the pipe cannot be created: the child then proceeds without waiting and simply
/// collects no threads, rather than blocking on a gate that will never open.
#[cfg(target_os = "linux")]
fn make_ptrace_gate() -> Option<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe() fills the two-element array and is async-signal-safe.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    Some((fds[0], fds[1]))
}

#[cfg(target_os = "linux")]
fn current_tid() -> libc::pid_t {
    // Prefer the raw syscall to avoid linking against libc's gettid symbol on glibc versions
    // where it may not be exposed.
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

#[cfg(not(target_os = "linux"))]
fn current_tid() -> libc::pid_t {
    0
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_collector_child(
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_str: &str,
    message: Option<&str>,
    crash: CrashKindData,
    uds_fd: RawFd,
    ppid: libc::pid_t,
    crashing_tid: libc::pid_t,
    ptrace_gate: Option<(RawFd, RawFd)>,
    threads_deadline: Instant,
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

    // Wait for the crashing process to grant us ptrace permission before touching its
    // threads; it closes the write end once the grant is in place.
    #[cfg(target_os = "linux")]
    if let Some((read_fd, write_fd)) = ptrace_gate {
        // SAFETY: read and close are async-signal-safe, and these fds are ours after the fork.
        unsafe {
            libc::close(write_fd);
            let mut byte = 0u8;
            while libc::read(read_fd, &mut byte as *mut u8 as *mut libc::c_void, 1) > 0 {}
            libc::close(read_fd);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ptrace_gate;

    // Emit crashreport
    let mut unix_stream = unsafe { UnixStream::from_raw_fd(uds_fd) };

    let report = emit_crashreport(
        &mut unix_stream,
        config,
        config_str,
        metadata_str,
        message,
        crash,
        ppid,
        crashing_tid,
        threads_deadline,
    );
    if let Err(e) = report {
        eprintln!("Failed to flush crash report: {e}");
        terminate();
    }

    // Exit normally
    unsafe { libc::_exit(0) };
}
