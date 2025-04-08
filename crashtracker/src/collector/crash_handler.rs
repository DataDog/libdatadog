// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
#![allow(deprecated)]

use super::emitters::emit_crashreport;
use super::saguard::SaGuard;
use super::signal_handler_manager::chain_signal_handler;
use crate::crash_info::Metadata;
use crate::shared::configuration::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use crate::shared::constants::*;
use anyhow::Context;
use libc::{c_void, execve, nfds_t, siginfo_t, ucontext_t};
use libc::{poll, pollfd, POLLHUP};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet};
use nix::sys::socket;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, Pid};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::{
    io::{FromRawFd, IntoRawFd, RawFd},
    net::UnixStream,
};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicPtr, AtomicU64};
use std::time::{Duration, Instant};

// Note that this file makes use the following async-signal safe functions in a signal handler.
// <https://man7.org/linux/man-pages/man7/signal-safety.7.html>
// - clock_gettime
// - close (although Rust may call `free` because we call the higher-level nix interface)
// - dup2
// - fork (but specifically only because it does so without calling atfork handlers)
// - kill
// - poll
// - raise
// - read
// - sigaction
// - write

// The use of fork or vfork is influenced by the availability of the function in the host libc.
// Macos seems to have deprecated vfork.  The reason to prefer vfork is to suppress atfork
// handlers.  This is OK because macos is primarily a test platform, and we have system-level
// testing on Linux in various CI environments.
#[cfg(target_os = "macos")]
use libc::fork as vfork;

#[cfg(target_os = "linux")]
use libc::vfork;

struct Receiver {
    receiver_uds: RawFd,
    receiver_pid: i32,
    oneshot: bool,
}

// The args_cstrings and env_vars_strings fields are just storage.  Even though they're
// unreferenced, they're a necessary part of the struct.
#[allow(dead_code)]
struct PreparedExecve {
    binary_path: CString,
    args_cstrings: Vec<CString>,
    args_ptrs: Vec<*const libc::c_char>,
    env_vars_cstrings: Vec<CString>,
    env_vars_ptrs: Vec<*const libc::c_char>,
}

impl PreparedExecve {
    fn new(config: &CrashtrackerReceiverConfig) -> Self {
        // Allocate and store binary path
        #[allow(clippy::expect_used)]
        let binary_path = CString::new(config.path_to_receiver_binary.as_str())
            .expect("Failed to convert binary path to CString");

        // Allocate and store arguments
        #[allow(clippy::expect_used)]
        let args_cstrings: Vec<CString> = config
            .args
            .iter()
            .map(|s| CString::new(s.as_str()).expect("Failed to convert argument to CString"))
            .collect();
        let args_ptrs: Vec<*const libc::c_char> = args_cstrings
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        // Allocate and store environment variables
        let env_vars_cstrings: Vec<CString> = config
            .env
            .iter()
            .map(|(key, value)| {
                let env_str = format!("{key}={value}");
                #[allow(clippy::expect_used)]
                CString::new(env_str).expect("Failed to convert environment variable to CString")
            })
            .collect();
        let env_vars_ptrs: Vec<*const libc::c_char> = env_vars_cstrings
            .iter()
            .map(|env| env.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        Self {
            binary_path,
            args_cstrings,
            args_ptrs,
            env_vars_cstrings,
            env_vars_ptrs,
        }
    }
}

/// Opens a file for writing (in append mode) or opens /dev/null
/// * If the filename is provided, it will try to open (creating if needed) the specified file.
///   Failure to do so is an error.
/// * If the filename is not provided, it will open /dev/null Some systems can fail to provide
///   `/dev/null` (e.g., chroot jails), so this failure is also an error.
/// * Using Stdio::null() is more direct, but it will cause a panic in environments where /dev/null
///   is not available.
fn open_file_or_quiet(filename: Option<&str>) -> anyhow::Result<RawFd> {
    let file = filename.map_or_else(
        || File::open("/dev/null").context("Failed to open /dev/null"),
        |f| {
            OpenOptions::new()
                .append(true)
                .create(true)
                .open(f)
                .with_context(|| format!("Failed to open or create file: {f}"))
        },
    )?;
    Ok(file.into_raw_fd())
}

/// Non-blocking child reaper
/// * If the child process has exited, return true
/// * If the child process cannot be found, return false
/// * If the child is still alive, or some other error occurs, return an error Either way, after
///   this returns, you probably don't have to do anything else.
// Note: some resources indicate it is unsafe to call `waitpid` from a signal handler, especially
//       on macos, where the OS will terminate an offending process.  This appears to be untrue
//       and `waitpid()` is characterized as async-signal safe by POSIX.
fn reap_child_non_blocking(pid: Pid, timeout_ms: u32) -> anyhow::Result<bool> {
    let timeout = Duration::from_millis(timeout_ms.into());
    let start_time = Instant::now();

    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => anyhow::ensure!(
                start_time.elapsed() <= timeout,
                "Timeout waiting for child process to exit"
            ),
            Ok(_status) => return Ok(true),
            Err(nix::Error::ECHILD) => {
                // Non-availability of the specified process is weird, since we should have
                // exclusive access to reaping its exit, but at the very least means there is
                // nothing further for us to do.
                return Ok(true);
            }
            _ => anyhow::bail!("Error waiting for child process to exit"),
        }
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

    // We've already prepared the arguments and environment variable
    // If we've reached this point, it means we've prepared the arguments and environment variables
    // ahead of time.  Extract them now.  This was prepared in advance in order to avoid heap
    // allocations in the signal handler.
    let receiver_args = RECEIVER_ARGS.load(SeqCst);
    let (binary_path, args_ptrs, env_vars_ptrs) = unsafe {
        #[allow(clippy::expect_used)]
        let receiver_args = receiver_args.as_ref().expect("No receiver arguments");
        (
            &receiver_args.binary_path,
            &receiver_args.args_ptrs,
            &receiver_args.env_vars_ptrs,
        )
    };

    // Before we actually execve, let's make sure that the signal handler in the receiver is set to
    // a default disposition.
    let sig_action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    unsafe {
        // If this fails there isn't much we can do, so just try anyway.
        let _ = signal::sigaction(signal::SIGCHLD, &sig_action);
    }

    // Change into the crashtracking receiver
    unsafe {
        execve(
            binary_path.as_ptr(),
            args_ptrs.as_ptr(),
            env_vars_ptrs.as_ptr(),
        );
    }

    // If we reach this point, execve failed, so just exit
    unsafe {
        libc::_exit(-1);
    }
}

/// true if successful wait, false if timeout occurred.
fn wait_for_pollhup(target_fd: RawFd, timeout_ms: i32) -> anyhow::Result<bool> {
    let mut poll_fds = [pollfd {
        fd: target_fd,
        events: POLLHUP,
        revents: 0,
    }];

    match unsafe { poll(poll_fds.as_mut_ptr(), poll_fds.len() as nfds_t, timeout_ms) } {
        -1 => Err(anyhow::anyhow!(
            "poll failed with errno: {}",
            std::io::Error::last_os_error()
        )),
        0 => Ok(false), // Timeout occurred
        _ => {
            let revents = poll_fds[0].revents;
            anyhow::ensure!(
                revents & POLLHUP != 0,
                "poll returned unexpected result: revents = {revents}"
            );
            Ok(true) // POLLHUP detected
        }
    }
}

// These represent data used by the crashtracker.
// Using mutexes inside a signal handler is not allowed, so use `AtomicPtr`
// instead to get atomicity.
// These should always be either: null_mut, or `Box::into_raw()`
// This means that we can always clean up the memory inside one of these using
// `Box::from_raw` to recreate the box, then dropping it.
static METADATA: AtomicPtr<(Metadata, String)> = AtomicPtr::new(ptr::null_mut());
static CONFIG: AtomicPtr<(CrashtrackerConfiguration, String)> = AtomicPtr::new(ptr::null_mut());
static RECEIVER_CONFIG: AtomicPtr<CrashtrackerReceiverConfig> = AtomicPtr::new(ptr::null_mut());
static RECEIVER_ARGS: AtomicPtr<PreparedExecve> = AtomicPtr::new(ptr::null_mut());

fn make_receiver(config: &CrashtrackerReceiverConfig) -> anyhow::Result<Receiver> {
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

/// Updates the crashtracker metadata for this process
/// Metadata is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_metadata(metadata: Metadata) -> anyhow::Result<()> {
    let metadata_string = serde_json::to_string(&metadata)?;
    let box_ptr = Box::into_raw(Box::new((metadata, metadata_string)));
    let old = METADATA.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
}

/// Updates the crashtracker config for this process
/// Config is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_config(config: CrashtrackerConfiguration) -> anyhow::Result<()> {
    let config_string = serde_json::to_string(&config)?;
    let box_ptr = Box::into_raw(Box::new((config, config_string)));
    let old = CONFIG.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
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
    let prepared_execve = PreparedExecve::new(&config);
    let box_ptr = Box::into_raw(Box::new(prepared_execve));
    let old = RECEIVER_ARGS.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
}

pub(crate) extern "C" fn handle_posix_sigaction(
    signum: i32,
    sig_info: *mut siginfo_t,
    ucontext: *mut c_void,
) {
    // Handle the signal.  Note this has a guard to ensure that we only generate
    // one crash report per process.
    let _ = handle_posix_signal_impl(sig_info, ucontext as *mut ucontext_t);
    // SAFETY: No preconditions.
    unsafe { chain_signal_handler(signum, sig_info, ucontext) };
}

fn receiver_from_socket(unix_socket_path: &str) -> anyhow::Result<Receiver> {
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

fn receiver_finish(receiver: Receiver, start_time: Instant, timeout_ms: u32) {
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

fn handle_posix_signal_impl(
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
) -> anyhow::Result<()> {
    // If this is a SIGSEGV signal, it could be called due to a stack overflow. In that case, since
    // this signal allocates to the stack and cannot guarantee it is running without SA_NODEFER, it
    // is possible that we will re-emit the signal. Contemporary unices handle this just fine (no
    // deadlock), but it does mean we will fail.  Currently this situation is not detected.
    // In general, handlers do not know their own stack usage requirements in advance and are
    // incapable of guaranteeing that they will not overflow the stack.

    // One-time guard to guarantee at most one crash per process
    static NUM_TIMES_CALLED: AtomicU64 = AtomicU64::new(0);
    if NUM_TIMES_CALLED.fetch_add(1, SeqCst) > 0 {
        // In the case where some lower-level signal handler recovered the error
        // we don't want to spam the system with calls.  Make this one shot.
        return Ok(());
    }

    // Leak config and metadata to avoid calling `drop` during a crash
    // Note that these operations also replace the global states.  When the one-time guard is
    // passed, all global configuration and metadata becomes invalid.
    // In a perfet world, we'd also grab the receiver config in this section, but since the
    // execution forks based on whether or not the receiver is configured, we check that later.
    let config = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { config.as_ref().context("No crashtracking receiver")? };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { metadata_ptr.as_ref().context("metadata ptr")? };

    let receiver_config = RECEIVER_CONFIG.load(SeqCst);
    if receiver_config.is_null() {
        return Err(anyhow::anyhow!("No receiver config"));
    }

    // Since we've gotten this far, we're going to start working on the crash report. This
    // operation needs to be mindful of the total walltime elapsed during handling. This isn't only
    // to prevent hanging, but also because services capable of restarting after a crash experience
    // crashes as probabalistic queue-holding events, and so crash handling represents dead time
    // which makes the overall service increasingly incompetent at handling load.
    let timeout_ms = config.timeout_ms();
    let start_time = Instant::now(); // This is the time at which the signal was received

    // During the execution of this signal handler, block ALL other signals, especially because we
    // cannot control whether or not we run with SA_NODEFER (crashtracker might have been chained).
    // The especially problematic signals are SIGCHLD and SIGPIPE, which are possibly delivered due
    // to the execution of this handler.
    // SaGuard ensures that signals are restored to their original state even if control flow is
    // disrupted.
    let _guard = SaGuard::<2>::new(&[signal::SIGCHLD, signal::SIGPIPE])?;

    // Optionally, create the receiver.  This all hinges on whether or not the configuration has a
    // non-null unix domain socket specified.  If it doesn't, then we need to check the receiver
    // configuration.  If it does, then we just connect to the socket.
    let unix_socket_path = config.unix_socket_path().clone().unwrap_or_default();

    let receiver = if !unix_socket_path.is_empty() {
        receiver_from_socket(&unix_socket_path)?
    } else {
        let receiver_config = RECEIVER_CONFIG.load(SeqCst);
        if receiver_config.is_null() {
            return Err(anyhow::anyhow!("No receiver config"));
        }
        let receiver_config = unsafe { receiver_config.as_ref().context("receiver config")? };
        make_receiver(receiver_config)?
    };

    // No matter how the receiver was created, attach to its stream
    let mut unix_stream = unsafe { UnixStream::from_raw_fd(receiver.receiver_uds) };

    // Currently the emission of the crash report doesn't have a firm time guarantee
    // In a future patch, the timeout parameter should be passed into the IPC loop here and
    // checked periodically.
    let res = emit_crashreport(
        &mut unix_stream,
        config,
        config_str,
        metadata_string,
        sig_info,
        ucontext,
    );

    let _ = unix_stream.flush();
    unix_stream
        .shutdown(std::net::Shutdown::Write)
        .context("Could not shutdown writing on the stream")?;

    // We're done. Wrap up our interaction with the receiver.
    receiver_finish(receiver, start_time, timeout_ms);

    res
}
