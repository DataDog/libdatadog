// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crash data collector process management for Unix socket communication.
//!
//! This module manages the collector process that writes crash data to Unix sockets.
//! The collector runs in a forked child process and is responsible for serializing
//! and transmitting crash information to the receiver process.
//!
//! ## Communication Flow (Collector Side)
//!
//! The collector performs these steps to transmit crash data:
//!
//! 1. **Process Setup**: Forks from crashing process, closes stdio, disables SIGPIPE
//! 2. **Socket Creation**: Creates `UnixStream` from inherited file descriptor
//! 3. **Data Serialization**: Calls [`emit_crashreport()`] to write structured crash data
//! 4. **Graceful Exit**: Flushes data and exits with `libc::_exit(0)`
//!
//! ```text
//! ┌─────────────────────┐                     ┌──────────────────────┐
//! │ Signal Handler      │                     │ Collector Process    │
//! │ (Original Process)  │                     │ (Forked Child)       │
//! │                     │                     │                      │
//! │ 1. Catch crash      │────fork()──────────►│ 2. Setup stdio       │
//! │ 2. Fork collector   │                     │ 3. Create UnixStream  │
//! │ 3. Wait for child   │                     │ 4. Write crash data   │
//! │                     │◄────wait()──────────│ 5. Exit cleanly      │
//! └─────────────────────┘                     └──────────────────────┘
//! ```
//!
//! ## Signal Safety
//!
//! All collector operations use only async-signal-safe functions since the collector
//! runs in a signal handler context:
//!
//! - No memory allocations
//! - Pre-prepared data structures
//! - Only safe system calls
//!
//! For complete protocol documentation, see [`crate::shared::unix_socket_communication`].
//!
//! [`emit_crashreport()`]: crate::collector::emitters::emit_crashreport

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
    /// Spawns a collector process to write crash data to the Unix socket.
    ///
    /// This method forks a child process that will serialize and transmit crash data
    /// to the receiver process via the Unix socket established in the receiver.
    ///
    /// ## Process Architecture
    ///
    /// ```text
    /// Parent Process (Signal Handler)    Child Process (Collector)
    /// ┌─────────────────────────────┐   ┌─────────────────────────────┐
    /// │ 1. Catches crash signal     │   │ 4. Closes stdio (0,1,2)     │
    /// │ 2. Forks collector process  │──►│ 5. Disables SIGPIPE         │
    /// │ 3. Returns to caller        │   │ 6. Creates UnixStream        │
    /// │                             │   │ 7. Calls emit_crashreport()  │
    /// │                             │   │ 8. Exits with _exit(0)      │
    /// └─────────────────────────────┘   └─────────────────────────────┘
    /// ```
    ///
    /// ## Arguments
    ///
    /// * `receiver` - The receiver process that will read crash data from the Unix socket
    /// * `config` - Crash tracker configuration
    /// * `config_str` - JSON-serialized configuration string
    /// * `metadata_str` - JSON-serialized metadata string
    /// * `sig_info` - Signal information from the crash
    /// * `ucontext` - Process context at crash time
    ///
    /// ## Returns
    ///
    /// * `Ok(Collector)` - Handle to the spawned collector process
    /// * `Err(CollectorSpawnError::ForkFailed)` - If the fork operation fails
    ///
    /// ## Safety
    ///
    /// This function is called from signal handler context and uses only async-signal-safe operations.
    /// The child process performs all potentially unsafe operations after fork.
    pub(crate) fn spawn(
        receiver: &Receiver,
        config: &CrashtrackerConfiguration,
        config_str: &str,
        metadata_str: &str,
        sig_info: *const siginfo_t,
        ucontext: *const ucontext_t,
    ) -> Result<Self, CollectorSpawnError> {
        // When we spawn the child, our pid becomes the ppid for process tracking.
        // SAFETY: getpid() is async-signal-safe.
        let pid = unsafe { libc::getpid() };

        let fork_result = alt_fork();
        match fork_result {
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

/// Collector child process entry point - serializes and transmits crash data via Unix socket.
///
/// This function runs in the forked collector process and performs the actual crash data
/// transmission. It establishes the Unix socket connection and writes all crash information
/// using the structured protocol.
///
/// ## Process Flow
///
/// 1. **Isolate from parent**: Closes stdin, stdout, stderr to prevent interference
/// 2. **Signal handling**: Disables SIGPIPE to handle broken pipe gracefully
/// 3. **Socket setup**: Creates `UnixStream` from inherited file descriptor
/// 4. **Data transmission**: Calls [`emit_crashreport()`] to write structured crash data
/// 5. **Clean exit**: Exits with `_exit(0)` to avoid cleanup issues
///
/// ## Communication Protocol
///
/// The crash data is written as a structured stream with delimited sections:
/// - Metadata, Configuration, Signal Info, Process Context
/// - Counters, Spans, Tags, Traces, Memory Maps, Stack Trace
/// - Completion marker
///
/// For details, see [`crate::shared::unix_socket_communication`].
///
/// ## Arguments
///
/// * `config` - Crash tracker configuration object
/// * `config_str` - JSON-serialized configuration for receiver
/// * `metadata_str` - JSON-serialized metadata for receiver
/// * `sig_info` - Signal information from crash context
/// * `ucontext` - Processor context at crash time
/// * `uds_fd` - Unix socket file descriptor for writing crash data
/// * `ppid` - Parent process ID for identification
///
/// This function never returns - it always exits via `_exit(0)` or `terminate()`.
///
/// [`emit_crashreport()`]: crate::collector::emitters::emit_crashreport
pub(crate) fn run_collector_child(
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_str: &str,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
    uds_fd: RawFd,
    ppid: libc::pid_t,
) -> ! {
    // Close stdio to isolate from parent process and prevent interference with crash data transmission
    let _ = unsafe { libc::close(0) };  // stdin
    let _ = unsafe { libc::close(1) };  // stdout
    let _ = unsafe { libc::close(2) };  // stderr

    // Disable SIGPIPE - if receiver closes socket early, we want to handle it gracefully
    // rather than being killed by SIGPIPE
    let _ = unsafe {
        signal::sigaction(
            signal::SIGPIPE,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )
    };

    // Create Unix socket stream for crash data transmission
    let mut unix_stream = unsafe { UnixStream::from_raw_fd(uds_fd) };

    // Serialize and transmit all crash data using structured protocol
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
