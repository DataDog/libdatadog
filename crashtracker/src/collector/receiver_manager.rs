// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
// This is needed for vfork.  Using vfork is removed on mac and deprecated on linux
// https://github.com/rust-lang/libc/issues/1596
// TODO: This is a problem, we should fix it.
#![allow(deprecated)]

use super::emitters::emit_crashreport;
use crate::shared::configuration::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use crate::shared::constants::*;
use anyhow::Context;
use ddcommon::unix_utils::{
    alt_fork, open_file_or_quiet, reap_child_non_blocking, terminate, wait_for_pollhup,
    PreparedExecve,
};
use libc::{siginfo_t, ucontext_t};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use nix::sys::socket;
use nix::unistd::Pid;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::os::unix::{io::FromRawFd, net::UnixStream};
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Instant;

static RECEIVER_CONFIG: AtomicPtr<(CrashtrackerReceiverConfig, PreparedExecve)> =
    AtomicPtr::new(ptr::null_mut());

pub(crate) struct WatchedProcess {
    uds_fd: RawFd,
    pid: i32,
    oneshot: bool,
}

impl WatchedProcess {
    pub(crate) fn finish(self, start_time: Instant, timeout_ms: u32) {
        let pollhup_allowed_ms = timeout_ms
            .saturating_sub(start_time.elapsed().as_millis() as u32)
            .min(i32::MAX as u32) as i32;
        let _ = wait_for_pollhup(self.uds_fd, pollhup_allowed_ms);

        if self.oneshot {
            // If we have less than the minimum amount of time, give ourselves a few scheduler
            // slices worth of headroom to help guarantee that we don't leak a zombie process.
            let _ = unsafe { libc::kill(self.pid, libc::SIGKILL) };
            let reaping_allowed_ms = std::cmp::min(
                timeout_ms.saturating_sub(start_time.elapsed().as_millis() as u32),
                DD_CRASHTRACK_MINIMUM_REAP_TIME_MS,
            );
            let _ = reap_child_non_blocking(Pid::from_raw(self.pid), reaping_allowed_ms);
        }
    }

    /// # Safety:
    ///   uds_fd should maintain the requirements of UnixStream::from_raw_fd.
    ///   The safety comment there seems to imply that the main thing is that the fd must
    ///   be open.  But they could be clearer about what they mean.
    ///   https://doc.rust-lang.org/std/os/fd/trait.FromRawFd.html
    ///   It is an error to call this twice
    pub(crate) fn from_socket(unix_socket_path: &str) -> anyhow::Result<Self> {
        // Creates a fake "Receiver", which can be waited on like a normal receiver.
        // This is intended to support configurations where the collector is speaking to a
        // long-lived, async receiver process.
        if unix_socket_path.is_empty() {
            return Err(anyhow::anyhow!("No receiver path provided"));
        }
        #[cfg(target_os = "linux")]
        let unix_stream = if unix_socket_path.starts_with(['.', '/']) {
            UnixStream::connect(unix_socket_path)
        } else {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(unix_socket_path)?;
            UnixStream::connect_addr(&addr)
        };
        #[cfg(not(target_os = "linux"))]
        let unix_stream = UnixStream::connect(unix_socket_path);
        let uds_fd = unix_stream
            .context("Failed to connect to receiver")?
            .into_raw_fd();
        Ok(Self {
            uds_fd,
            pid: 0,
            oneshot: false,
        })
    }

    pub(crate) fn from_config(
        config: &CrashtrackerReceiverConfig,
        prepared_exec: &PreparedExecve,
    ) -> anyhow::Result<Self> {
        let stderr = open_file_or_quiet(config.stderr_filename.as_deref())?;
        let stdout = open_file_or_quiet(config.stdout_filename.as_deref())?;

        // Create anonymous Unix domain socket pair for communication
        let (uds_parent, uds_child) = socket::socketpair(
            socket::AddressFamily::Unix,
            socket::SockType::Stream,
            None,
            socket::SockFlag::empty(),
        )
        .context("Failed to create Unix domain socket pair")
        .map(|(a, b)| (a.into_raw_fd(), b.into_raw_fd()))?;

        // See ddcommon::unix_utils for platform-specific comments on alt_fork()
        match alt_fork() {
            0 => {
                // Child (noreturn)
                let _ = unsafe { libc::close(uds_parent) };
                run_receiver_child(prepared_exec, uds_child, stderr, stdout)
            }
            pid if pid > 0 => {
                // Parent
                let _ = unsafe { libc::close(uds_child) };
                Ok(WatchedProcess {
                    uds_fd: uds_parent,
                    pid,
                    oneshot: true,
                })
            }
            _ => {
                // Error
                Err(anyhow::anyhow!("Failed to create receiver process"))
            }
        }
    }

    pub(crate) fn to_collector(
        &self,
        config: &CrashtrackerConfiguration,
        config_str: &str,
        metadata_str: &str,
        sig_info: *const siginfo_t,
        ucontext: *const ucontext_t,
    ) -> anyhow::Result<Self> {
        let ppid = unsafe { libc::getppid() };

        match alt_fork() {
            0 => {
                // Child (does not exit from this function)
                run_collector_child(
                    config,
                    config_str,
                    metadata_str,
                    sig_info,
                    ucontext,
                    self,
                    ppid,
                );
            }
            pid if pid > 0 => Ok(WatchedProcess {
                uds_fd: self.uds_fd,
                pid,
                oneshot: true,
            }),
            _ => {
                // Error
                Err(anyhow::anyhow!("Failed to fork collector process"))
            }
        }
    }

    pub(crate) fn from_stored_config() -> anyhow::Result<Self> {
        let receiver_config = RECEIVER_CONFIG.swap(ptr::null_mut(), SeqCst);
        anyhow::ensure!(!receiver_config.is_null(), "No receiver config");
        // Intentionally leak since we're in a signal handler
        let (config, prepared_exec) =
            unsafe { receiver_config.as_ref().context("receiver config")? };
        Self::from_config(config, prepared_exec)
    }

    /// Ensures that the receiver has the configuration when it starts.
    /// PRECONDITIONS:
    ///    None
    /// SAFETY:
    ///   This function is not reentrant.
    ///   No other crash-handler functions should be called concurrently.
    /// ATOMICITY:
    ///     This function uses a swap on an atomic pointer.
    pub fn update_stored_config(config: CrashtrackerReceiverConfig) {
        // Heap-allocate the parts of the configuration that relate to execve.
        // This needs to be done because the execve call requires a specific layout, and achieving
        // this layout requires allocations.  We should strive not to allocate from within a
        // signal handler, so we do it now.
        let prepared_execve =
            PreparedExecve::new(&config.path_to_receiver_binary, &config.args, &config.env);
        // First, propagate the configuration
        let box_ptr = Box::into_raw(Box::new((config, prepared_execve)));
        let old = RECEIVER_CONFIG.swap(box_ptr, SeqCst);
        if !old.is_null() {
            // Safety: This can only come from a box above.
            unsafe {
                std::mem::drop(Box::from_raw(old));
            }
        }
    }
}

/// Wrapper around the child process that will run the crash receiver
fn run_receiver_child(
    prepared_exec: &PreparedExecve,
    uds_child: RawFd,
    stderr: RawFd,
    stdout: RawFd,
) -> ! {
    // File descriptor management
    unsafe {
        let _ = libc::dup2(uds_child, 0);
        let _ = libc::dup2(stdout, 1);
        let _ = libc::dup2(stderr, 2);
    }

    // Close unused file descriptors
    let _ = unsafe { libc::close(uds_child) };
    let _ = unsafe { libc::close(stderr) };
    let _ = unsafe { libc::close(stdout) };

    // Before we actually execve, let's make sure that the signal handler in the receiver is set to
    // a default disposition.
    let sig_action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    unsafe {
        // If this fails there isn't much we can do, so just try anyway.
        let _ = signal::sigaction(signal::SIGCHLD, &sig_action);
    }

    // We've already prepared the arguments and environment variable
    // If we've reached this point, it means we've prepared the arguments and environment variables
    // ahead of time.  Extract them now.  This was prepared in advance in order to avoid heap
    // allocations in the signal handler.
    // We intentionally leak the memory since we're in a crashing signal handler.
    // Safety: this pointer is either NULL, or came from a box that has not been dropped.
    prepared_exec.exec().unwrap_or_else(|_| terminate());
    // If we reach this point, execve failed, so just exit
    terminate();
}

pub(crate) fn run_collector_child(
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_str: &str,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
    receiver: &WatchedProcess,
    ppid: libc::pid_t,
) -> ! {
    // the collector process currently runs without access to stdio.  there are two ways to resolve
    // this:
    // - reuse the stdio files used for the receiver
    //   + con: we don't controll flushes to stdio, so the writes from the two processes may
    //     interleave parts of a single message
    // - create two new stdio files for the collector
    //   + con: that's _another_ two options to fill out in the config
    let _ = unsafe { libc::close(0) };
    let _ = unsafe { libc::close(1) };
    let _ = unsafe { libc::close(2) };

    // Before we start reading or writing to the socket, we need to disable SIGPIPE because we
    // don't control the emission implementation enough to send MSG_NOSIGNAL.
    // NB - collector is running in its own process, so this doesn't affect the watchdog process
    let _ = unsafe {
        signal::sigaction(
            signal::SIGPIPE,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )
    };

    // We're ready to emit the crashreport
    let mut unix_stream = unsafe { UnixStream::from_raw_fd(receiver.uds_fd) };

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
        unsafe { libc::_exit(-1) };
    }

    // If we reach this point, then we exit.  We're done.
    // Note that since we're a forked process, we call `_exiit` in favor of:
    // - `exit()`, because calling the atexit handlers might mutate shared state
    // - `abort()`, because raising SIGABRT might cause other problems
    unsafe { libc::_exit(0) };
}
