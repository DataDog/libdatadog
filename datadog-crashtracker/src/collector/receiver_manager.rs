// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::process_handle::ProcessHandle;
use ddcommon::timeout::TimeoutManager;

use crate::shared::configuration::CrashtrackerReceiverConfig;
use anyhow::Context;
use ddcommon::unix_utils::{alt_fork, open_file_or_quiet, terminate, PreparedExecve};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use nix::sys::socket;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::SeqCst;

static RECEIVER_CONFIG: AtomicPtr<(CrashtrackerReceiverConfig, PreparedExecve)> =
    AtomicPtr::new(ptr::null_mut());

pub(crate) struct Receiver {
    pub handle: ProcessHandle,
}

impl Receiver {
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
            handle: ProcessHandle::new(uds_fd, None),
        })
    }

    pub(crate) fn spawn_from_config(
        config: &CrashtrackerReceiverConfig,
        prepared_exec: &PreparedExecve,
    ) -> anyhow::Result<Self> {
        let stderr = open_file_or_quiet(config.stderr_filename.as_deref())
            .context("Failed to open stderr file")?;
        let stdout = open_file_or_quiet(config.stdout_filename.as_deref())
            .context("Failed to open stdout file")?;

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
                Ok(Self {
                    handle: ProcessHandle::new(uds_parent, Some(pid)),
                })
            }
            _ => {
                // Error
                Err(anyhow::anyhow!("Failed to create receiver process"))
            }
        }
    }

    pub(crate) fn spawn_from_stored_config() -> anyhow::Result<Self> {
        let receiver_config = RECEIVER_CONFIG.swap(ptr::null_mut(), SeqCst);
        anyhow::ensure!(!receiver_config.is_null(), "No receiver config");
        // Intentionally leak since we're in a signal handler
        let (config, prepared_exec) =
            unsafe { receiver_config.as_ref().context("receiver config")? };
        Self::spawn_from_config(config, prepared_exec)
    }

    /// Ensures that the receiver has the configuration when it starts.
    /// PRECONDITIONS:
    ///    None
    /// SAFETY:
    ///   This function is not reentrant.
    ///   No other crash-handler functions should be called concurrently.
    /// ATOMICITY:
    ///     This function uses a swap on an atomic pointer.
    pub fn update_stored_config(
        config: CrashtrackerReceiverConfig,
    ) -> Result<(), ddcommon::unix_utils::PreparedExecveError> {
        // Heap-allocate the parts of the configuration that relate to execve.
        // This needs to be done because the execve call requires a specific layout, and achieving
        // this layout requires allocations.  We should strive not to allocate from within a
        // signal handler, so we do it now.
        let prepared_execve =
            PreparedExecve::new(&config.path_to_receiver_binary, &config.args, &config.env)?;
        // First, propagate the configuration
        let box_ptr = Box::into_raw(Box::new((config, prepared_execve)));
        let old = RECEIVER_CONFIG.swap(box_ptr, SeqCst);
        if !old.is_null() {
            // Safety: This can only come from a box above.
            unsafe {
                std::mem::drop(Box::from_raw(old));
            }
        }
        Ok(())
    }

    pub fn finish(self, timeout_manager: &TimeoutManager) {
        self.handle.finish(timeout_manager);
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
