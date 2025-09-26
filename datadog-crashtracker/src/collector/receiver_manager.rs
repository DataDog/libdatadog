// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crash tracker receiver process management and Unix socket communication setup.
//!
//! This module handles the creation and management of receiver processes that collect
//! crash data via Unix domain sockets. The crash tracker uses a two-process architecture:
//!
//! 1. **Collector Process**: Forks from the crashing process and writes crash data to a Unix socket
//! 2. **Receiver Process**: Created via fork+execve, reads from Unix socket and processes/uploads crash data
//!
//! ## Socket Communication Architecture
//!
//! The communication uses anonymous Unix domain socket pairs created with [`socketpair()`]:
//!
//! ```text
//! ┌─────────────────┐    socketpair()    ┌─────────────────┐
//! │ Collector       │◄───────────────────►│ Receiver        │
//! │ (Crashing proc) │                     │ (Fork+execve)   │
//! │                 │     Write End       │                 │
//! │ uds_parent ────────────────────────────────► stdin (fd=0) │
//! └─────────────────┘                     └─────────────────┘
//! ```
//!
//! For complete protocol documentation, see [`crate::shared::unix_socket_communication`].
//!
//! [`socketpair()`]: nix::sys::socket::socketpair

use super::process_handle::ProcessHandle;
use ddcommon::timeout::TimeoutManager;

use crate::shared::configuration::CrashtrackerReceiverConfig;
use ddcommon::unix_utils::{alt_fork, open_file_or_quiet, terminate, PreparedExecve};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use nix::sys::socket;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::SeqCst;

#[derive(Debug, thiserror::Error)]
pub enum ReceiverError {
    #[error("No receiver path provided")]
    NoReceiverPath,
    #[error("Failed to connect to receiver: {0}")]
    ConnectionError(std::io::Error),
    #[error("Failed to create Unix domain socket pair: {0}")]
    SocketPairError(nix::Error),
    #[error("Failed to create receiver process (fork error code: {0})")]
    ForkFailed(i32),
    #[error("No receiver config available")]
    NoConfig,
    #[error("Failed to open file: {0}")]
    FileOpenError(std::io::Error),
    #[error("Failed to prepare execve: {0}")]
    PreparedExecveError(#[from] ddcommon::unix_utils::PreparedExecveError),
}

static RECEIVER_CONFIG: AtomicPtr<(CrashtrackerReceiverConfig, PreparedExecve)> =
    AtomicPtr::new(ptr::null_mut());

pub(crate) struct Receiver {
    pub handle: ProcessHandle,
}

impl Receiver {
    /// Creates a receiver that connects to an existing Unix socket.
    ///
    /// This mode is used when connecting to a long-lived receiver process instead of
    /// spawning a new one via fork+execve. The collector will write crash data directly
    /// to the provided Unix socket.
    ///
    /// ## Socket Path Formats
    ///
    /// - **File system sockets**: Paths starting with `.` or `/` (e.g., `/tmp/receiver.sock`)
    /// - **Abstract sockets** (Linux only): Names not starting with `.` or `/` (e.g., `crashtracker-receiver`)
    ///
    /// ## Arguments
    ///
    /// * `unix_socket_path` - Path to the Unix socket (file system or abstract name)
    ///
    /// ## Errors
    ///
    /// * [`ReceiverError::NoReceiverPath`] - If the path is empty
    /// * [`ReceiverError::ConnectionError`] - If connection to the socket fails
    pub(crate) fn from_socket(unix_socket_path: &str) -> Result<Self, ReceiverError> {
        if unix_socket_path.is_empty() {
            return Err(ReceiverError::NoReceiverPath);
        }
        #[cfg(target_os = "linux")]
        let unix_stream = if unix_socket_path.starts_with(['.', '/']) {
            UnixStream::connect(unix_socket_path)
        } else {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(unix_socket_path)
                .map_err(ReceiverError::ConnectionError)?;
            UnixStream::connect_addr(&addr)
        };
        #[cfg(not(target_os = "linux"))]
        let unix_stream = UnixStream::connect(unix_socket_path);
        let uds_fd = unix_stream
            .map_err(ReceiverError::ConnectionError)?
            .into_raw_fd();
        Ok(Self {
            handle: ProcessHandle::new(uds_fd, None),
        })
    }

    /// Spawns a new receiver process using fork+execve with Unix socket communication.
    ///
    /// This is the primary method for creating receiver processes. It:
    ///
    /// 1. **Creates socket pair**: Uses [`socketpair()`] to establish anonymous Unix domain socket communication
    /// 2. **Forks process**: Creates child process that will become the receiver
    /// 3. **Sets up file descriptors**: Redirects socket to stdin, configures stdout/stderr
    /// 4. **Executes receiver**: Child process executes the receiver binary
    ///
    /// ## Socket Architecture
    ///
    /// ```text
    /// Parent Process                    Child Process
    /// ┌─────────────────────┐          ┌─────────────────────┐
    /// │ Collector           │          │ Receiver            │
    /// │                     │          │                     │
    /// │ uds_parent (write) ─────────────────► stdin (fd=0)    │
    /// │                     │          │ stdout (configured) │
    /// │                     │          │ stderr (configured) │
    /// └─────────────────────┘          └─────────────────────┘
    /// ```
    ///
    /// ## File Descriptor Management
    ///
    /// - **Parent**: Keeps `uds_parent` for writing crash data
    /// - **Child**: `uds_child` redirected to stdin, original socket closed
    /// - **Stdio**: Child's stdout/stderr redirected to configured files or `/dev/null`
    ///
    /// ## Arguments
    ///
    /// * `config` - Receiver configuration including binary path and I/O redirection
    /// * `prepared_exec` - Pre-prepared execve arguments and environment (avoids allocations in signal handler)
    ///
    /// ## Errors
    ///
    /// * [`ReceiverError::FileOpenError`] - Failed to open stdout/stderr files
    /// * [`ReceiverError::SocketPairError`] - Failed to create socket pair
    /// * [`ReceiverError::ForkFailed`] - Fork operation failed
    ///
    /// [`socketpair()`]: nix::sys::socket::socketpair
    pub(crate) fn spawn_from_config(
        config: &CrashtrackerReceiverConfig,
        prepared_exec: &PreparedExecve,
    ) -> Result<Self, ReceiverError> {
        let stderr = open_file_or_quiet(config.stderr_filename.as_deref())
            .map_err(ReceiverError::FileOpenError)?;
        let stdout = open_file_or_quiet(config.stdout_filename.as_deref())
            .map_err(ReceiverError::FileOpenError)?;

        // Create anonymous Unix domain socket pair for communication between collector and receiver.
        // This establishes a bidirectional communication channel where:
        // - uds_parent: Used by collector (parent/grandparent) process for writing crash data
        // - uds_child: Used by receiver process, redirected to stdin for reading crash data
        let (uds_parent, uds_child) = socket::socketpair(
            socket::AddressFamily::Unix,
            socket::SockType::Stream,
            None,
            socket::SockFlag::empty(),
        )
        .map_err(ReceiverError::SocketPairError)?;
        let (uds_parent, uds_child) = (uds_parent.into_raw_fd(), uds_child.into_raw_fd());

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
            code => {
                // Error
                Err(ReceiverError::ForkFailed(code))
            }
        }
    }

    pub(crate) fn spawn_from_stored_config() -> Result<Self, ReceiverError> {
        let receiver_config = RECEIVER_CONFIG.swap(ptr::null_mut(), SeqCst);
        if receiver_config.is_null() {
            return Err(ReceiverError::NoConfig);
        }
        // Intentionally leak since we're in a signal handler
        let (config, prepared_exec) = unsafe { &*receiver_config };
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
    pub fn update_stored_config(config: CrashtrackerReceiverConfig) -> Result<(), ReceiverError> {
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

/// Child process entry point that sets up file descriptors and executes the receiver binary.
///
/// This function is called only in the child process after fork. It performs critical
/// file descriptor setup to establish the Unix socket communication channel:
///
/// ## File Descriptor Setup
///
/// 1. **stdin (fd=0)**: Redirected to `uds_child` socket for receiving crash data
/// 2. **stdout (fd=1)**: Redirected to configured output file or `/dev/null`
/// 3. **stderr (fd=2)**: Redirected to configured error file or `/dev/null`
///
/// ## Signal Handler Reset
///
/// Signal handlers are reset to default disposition to ensure clean receiver operation.
///
/// ## Arguments
///
/// * `prepared_exec` - Pre-prepared execve arguments and environment
/// * `uds_child` - Unix socket file descriptor for reading crash data
/// * `stderr` - File descriptor for stderr redirection
/// * `stdout` - File descriptor for stdout redirection
///
/// This function never returns - it either successfully executes the receiver binary
/// or terminates the process.
fn run_receiver_child(
    prepared_exec: &PreparedExecve,
    uds_child: RawFd,
    stderr: RawFd,
    stdout: RawFd,
) -> ! {
    // File descriptor management: Redirect Unix socket to stdin so receiver can read crash data
    unsafe {
        let _ = libc::dup2(uds_child, 0);   // stdin = Unix socket (crash data input)
        let _ = libc::dup2(stdout, 1);     // stdout = configured output file
        let _ = libc::dup2(stderr, 2);     // stderr = configured error file
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
