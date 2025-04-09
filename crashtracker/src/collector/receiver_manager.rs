// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
// This is needed for vfork.  Using vfork is removed on mac and deprecated on linux
// https://github.com/rust-lang/libc/issues/1596
// TODO: This is a problem, we should fix it.
#![allow(deprecated)]

use crate::shared::configuration::CrashtrackerReceiverConfig;
use crate::shared::constants::*;
use anyhow::Context;
use ddcommon::unix_utils::{
    open_file_or_quiet, reap_child_non_blocking, wait_for_pollhup, PreparedExecve,
};
use libc::{_exit, EXIT_FAILURE};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use nix::sys::socket;
use nix::unistd::{close, Pid};
use std::os::unix::io::{IntoRawFd, RawFd};
use std::os::unix::{io::FromRawFd, net::UnixStream};
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Instant;

static RECEIVER_ARGS: AtomicPtr<PreparedExecve> = AtomicPtr::new(ptr::null_mut());
static RECEIVER_CONFIG: AtomicPtr<CrashtrackerReceiverConfig> = AtomicPtr::new(ptr::null_mut());

// The use of fork or vfork is influenced by the availability of the function in the host libc.
// Macos seems to have deprecated vfork.  The reason to prefer vfork is to suppress atfork
// handlers.  This is OK because macos is primarily a test platform, and we have system-level
// testing on Linux in various CI environments.
#[cfg(target_os = "macos")]
use libc::fork as vfork;

#[cfg(target_os = "linux")]
use libc::vfork;

pub(crate) struct Receiver {
    receiver_uds: RawFd,
    receiver_pid: i32,
    oneshot: bool,
}

impl Receiver {
    /// Safety: `receiver_uds` should maintain the requirements of `UnixStream::from_raw_fd`.
    ///         The safety comment there seems to imply that the main thing is that the fd must
    ///         be open.
    pub(crate) unsafe fn receiver_unix_stream(&mut self) -> UnixStream {
        // Safety: precondition of this function
        unsafe { UnixStream::from_raw_fd(self.receiver_uds) }
    }
}

pub(crate) fn has_receiver_config() -> bool {
    !RECEIVER_CONFIG.load(SeqCst).is_null()
}

pub(crate) fn receiver_from_socket(unix_socket_path: &str) -> anyhow::Result<Receiver> {
    // Creates a fake "Receiver", which can be waited on like a normal receiver.
    // This is intended to support configurations where the collector is speaking to a long-lived,
    // async receiver process.
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
    let receiver_uds = unix_stream
        .context("Failed to connect to receiver")?
        .into_raw_fd();
    Ok(Receiver {
        receiver_uds,
        receiver_pid: 0,
        oneshot: false,
    })
}

pub(crate) fn receiver_finish(receiver: Receiver, start_time: Instant, timeout_ms: u32) {
    let pollhup_allowed_ms = timeout_ms
        .saturating_sub(start_time.elapsed().as_millis() as u32)
        .min(i32::MAX as u32) as i32;
    let _ = wait_for_pollhup(receiver.receiver_uds, pollhup_allowed_ms);

    // If this is a oneshot-type receiver (i.e., we spawned it), then we now need to ensure it gets
    // cleaned up.
    // We explicitly avoid the case where the receiver PID is 1.  This is unbelievably unlikely, but
    // should the situation arise we just walk away and let the PID leak.
    if receiver.oneshot && receiver.receiver_pid > 1 {
        // Either the receiver is done, it timed out, or something failed.
        // In any case, can't guarantee that the receiver will exit.
        // SIGKILL will ensure that the process ends eventually, but there's
        // no bound on that time.
        // We emit SIGKILL and try to reap its exit status for the remaining time, then give up.
        unsafe {
            libc::kill(receiver.receiver_pid, libc::SIGKILL);
        }

        let receiver_pid_as_pid = Pid::from_raw(receiver.receiver_pid);
        let reaping_allowed_ms = std::cmp::min(
            timeout_ms.saturating_sub(start_time.elapsed().as_millis() as u32),
            DD_CRASHTRACK_MINIMUM_REAP_TIME_MS,
        );

        let _ = reap_child_non_blocking(receiver_pid_as_pid, reaping_allowed_ms);
    }
}

/// Wrapper around the child process that will run the crash receiver
fn run_receiver_child(uds_parent: RawFd, uds_child: RawFd, stderr: RawFd, stdout: RawFd) -> ! {
    // File descriptor management
    unsafe {
        let _ = libc::dup2(uds_child, 0);
        let _ = libc::dup2(stdout, 1);
        let _ = libc::dup2(stderr, 2);
    }

    // Close unused file descriptors
    let _ = close(uds_parent);
    let _ = close(uds_child);
    let _ = close(stderr);
    let _ = close(stdout);

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
    unsafe {
        RECEIVER_ARGS
            .swap(ptr::null_mut(), SeqCst)
            .as_ref()
            .unwrap_or_else(|| _exit(EXIT_FAILURE))
            .exec()
            .unwrap_or_else(|_| _exit(EXIT_FAILURE));
    }
    // If we reach this point, execve failed, so just exit
    unsafe {
        _exit(EXIT_FAILURE);
    }
}

pub(crate) fn make_receiver() -> anyhow::Result<Receiver> {
    let receiver_config = RECEIVER_CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!receiver_config.is_null(), "No receiver config");
    // Intentionally leak since we're in a signal handler
    let config = unsafe { receiver_config.as_ref().context("receiver config")? };

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

    // We need to spawn a process without calling atfork handlers, since this is happening inside
    // of a signal handler.  Moreover, preference is given to multiplatform-uniform solutions.
    // Although `vfork()` is deprecated, the alternatives have limitations
    // * `fork()` calls atfork handlers
    // * There is no guarantee that `posix_spawn()` will not call `fork()` internally
    // * `clone()`/`clone3()` are Linux-specific
    // Accordingly, use `vfork()` for now
    // NB -- on macos the underlying implementation here is actually `fork()`!  See the top of this
    // file for details.
    match unsafe { vfork() } {
        0 => {
            // Child (noreturn)
            run_receiver_child(uds_parent, uds_child, stderr, stdout)
        }
        pid if pid > 0 => {
            // Parent
            let _ = close(uds_child);
            Ok(Receiver {
                receiver_uds: uds_parent,
                receiver_pid: pid,
                oneshot: true,
            })
        }
        _ => {
            // Error
            Err(anyhow::anyhow!("Failed to fork receiver process"))
        }
    }
}

/// Ensures that the receiver has the configuration when it starts.
/// PRECONDITIONS:
///    None
/// SAFETY:
///   This function is not reentrant.
///   No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn configure_receiver(config: CrashtrackerReceiverConfig) {
    // First, propagate the configuration
    let box_ptr = Box::into_raw(Box::new(config.clone()));
    let old = RECEIVER_CONFIG.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }

    // Next, heap-allocate the parts of the configuration that relate to execve
    // This needs to be done because the execve call requires a specific layout, and achieving this
    // layout requires allocations.  We should strive not to allocate from within a signal handler,
    // so we do it now.
    let prepared_execve =
        PreparedExecve::new(&config.path_to_receiver_binary, &config.args, &config.env);
    let box_ptr = Box::into_raw(Box::new(prepared_execve));
    let old = RECEIVER_ARGS.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
}
