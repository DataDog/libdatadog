// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
#![allow(deprecated)]

use super::emitters::emit_crashreport;
use super::saguard::SaGuard;
use crate::crash_info::CrashtrackerMetadata;
use crate::shared::configuration::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use crate::shared::constants::*;
use anyhow::Context;
use libc::{
    c_void, execve, mmap, nfds_t, sigaltstack, siginfo_t, MAP_ANON, MAP_FAILED, MAP_PRIVATE,
    PROT_NONE, PROT_READ, PROT_WRITE, SIGSTKSZ,
};
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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64};
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

#[derive(Debug)]
struct OldHandlers {
    pub sigbus: SigAction,
    pub sigsegv: SigAction,
}

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
        let binary_path = CString::new(config.path_to_receiver_binary.as_str())
            .expect("Failed to convert binary path to CString");

        // Allocate and store arguments
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
static ALTSTACK_INIT: AtomicBool = AtomicBool::new(false);
static OLD_HANDLERS: AtomicPtr<OldHandlers> = AtomicPtr::new(ptr::null_mut());
static METADATA: AtomicPtr<(CrashtrackerMetadata, String)> = AtomicPtr::new(ptr::null_mut());
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
pub fn update_metadata(metadata: CrashtrackerMetadata) -> anyhow::Result<()> {
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

extern "C" fn handle_posix_sigaction(signum: i32, sig_info: *mut siginfo_t, ucontext: *mut c_void) {
    // Handle the signal.  Note this has a guard to ensure that we only generate
    // one crash report per process.
    let _ = handle_posix_signal_impl(signum, sig_info);

    // Once we've handled the signal, chain to any previous handlers.
    // SAFETY: This was created by [register_crash_handlers].  There is a tiny
    // instant of time between when the handlers are registered, and the
    // `OLD_HANDLERS` are set.  This should be very short, but is hard to fully
    // eliminate given the existing POSIX APIs.
    let old_handlers = unsafe { &*OLD_HANDLERS.load(SeqCst) };
    let old_sigaction = if signum == libc::SIGSEGV {
        old_handlers.sigsegv
    } else if signum == libc::SIGBUS {
        old_handlers.sigbus
    } else {
        unreachable!("The only signals we're registered for are SEGV and BUS")
    };

    // How we chain depends on what kind of handler we're chaining to.
    // https://www.gnu.org/software/libc/manual/html_node/Signal-Handling.html
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // Follow the approach here:
    // https://stackoverflow.com/questions/6015498/executing-default-signal-handler
    match old_sigaction.handler() {
        SigHandler::SigDfl => {
            // In the case of a default handler, we want to invoke it so that
            // the core-dump can be generated.  Restoring the handler then
            // re-raising the signal accomplishes that.
            let signal = if signum == libc::SIGSEGV {
                signal::SIGSEGV
            } else if signum == libc::SIGBUS {
                signal::SIGBUS
            } else {
                unreachable!("The only signals we're registered for are SEGV and BUS")
            };
            unsafe { signal::sigaction(signal, &old_sigaction) }
                .unwrap_or_else(|_| std::process::abort());
            // Signals are only delivered once.
            // In the case where we were invoked because of a crash, returning
            // is technically UB but in practice re-invokes the crashing instr
            // and re-raises the signal. In the case where we were invoked by
            // `raise(SIGSEGV)` we need to re-raise the signal, or the default
            // handler will never receive it.
            unsafe { libc::raise(signum) };
        }
        SigHandler::SigIgn => (), // Return and ignore the signal.
        SigHandler::Handler(f) => f(signum),
        SigHandler::SigAction(f) => f(signum, sig_info, ucontext),
    };
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

fn handle_posix_signal_impl(signum: i32, sig_info: *mut siginfo_t) -> anyhow::Result<()> {
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
    let timeout_ms = config.timeout_ms;
    let start_time = Instant::now(); // This is the time at which the signal was received

    // Derive the faulting address from `sig_info`
    let faulting_address: Option<usize> =
        if !sig_info.is_null() && (signum == libc::SIGSEGV || signum == libc::SIGBUS) {
            unsafe { Some((*sig_info).si_addr() as usize) }
        } else {
            None
        };

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
    let unix_socket_path = config.unix_socket_path.clone().unwrap_or_default();

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
        signum,
        faulting_address,
    );

    let _ = unix_stream.flush();
    unix_stream
        .shutdown(std::net::Shutdown::Write)
        .context("Could not shutdown writing on the stream")?;

    // We're done. Wrap up our interaction with the receiver.
    receiver_finish(receiver, start_time, timeout_ms);

    res
}

/// Registers UNIX signal handlers to detect program crashes.
/// This function can be called multiple times and will be idempotent: it will
/// only create and set the handlers once.
/// However, note the restriction below:
/// PRECONDITIONS:
///     The signal handlers should be restored before removing the receiver.
///     `configure_receiver()` needs to be called before this function.
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a compare_and_exchange on an atomic pointer.
///     However, setting the crash handler itself is not an atomic operation
///     and hence it is possible that a concurrent operation could see partial
///     execution of this function.
///     If a crash occurs during execution of this function, it is possible that
///     the crash handler will have been registered, but the old signal handler
///     will not yet be stored.  This would lead to unexpected behaviour for the
///     user.  This should only matter if something crashes concurrently with
///     this function executing.
pub fn register_crash_handlers() -> anyhow::Result<()> {
    if !OLD_HANDLERS.load(SeqCst).is_null() {
        return Ok(());
    }

    let config_ptr = CONFIG.load(SeqCst);
    anyhow::ensure!(!config_ptr.is_null(), "No crashtracking config");
    let (config, _config_str) = unsafe { config_ptr.as_ref().context("config ptr")? };

    unsafe {
        if config.create_alt_stack {
            create_alt_stack()?;
        }
        let sigbus = register_signal_handler(signal::SIGBUS, config)?;
        let sigsegv = register_signal_handler(signal::SIGSEGV, config)?;
        let boxed_ptr = Box::into_raw(Box::new(OldHandlers { sigbus, sigsegv }));

        let res = OLD_HANDLERS.compare_exchange(ptr::null_mut(), boxed_ptr, SeqCst, SeqCst);
        anyhow::ensure!(
            res.is_ok(),
            "TOCTTOU error in crashtracker::register_crash_handlers"
        );
    }
    Ok(())
}

unsafe fn register_signal_handler(
    signal_type: signal::Signal,
    config: &CrashtrackerConfiguration,
) -> anyhow::Result<SigAction> {
    // Between this and `create_alt_stack()`, there are a few things going on.
    // - It is generally preferable to run in an altstack, given the choice.
    // - Crashtracking does not currently provide any particular guarantees around stack usage; in
    //   fact, it has been observed to frequently exceed 8192 bytes (default SIGSTKSZ) in practice.
    // - Some runtimes (Ruby) will set the altstack to a respectable size (~16k), but will check the
    //   value of the SP during their chained handler and become upset if the altstack is not what
    //   they expect--in these cases, it is necessary to USE the altstack without creating it.
    // - Some runtimes (Python, Rust) will set the altstack to the default size (8k), but will not
    //   check the value of the SP during their chained handler--in these cases, for correct
    //   operation it is necessary to CREATE and USE the altstack.
    // - There are no known cases where it is useful to crate but not use the altstack--this case
    //   handled in `new()` for CrashtrackerConfiguration.
    let extra_saflags = if config.use_alt_stack {
        SaFlags::SA_ONSTACK
    } else {
        SaFlags::empty()
    };

    let sig_action = SigAction::new(
        SigHandler::SigAction(handle_posix_sigaction),
        SaFlags::SA_NODEFER | extra_saflags,
        signal::SigSet::empty(),
    );

    let old_handler = signal::sigaction(signal_type, &sig_action)?;
    Ok(old_handler)
}

pub fn restore_old_handlers(inside_signal_handler: bool) -> anyhow::Result<()> {
    let prev = OLD_HANDLERS.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!prev.is_null(), "No crashtracking previous signal handlers");
    // Safety: The only nonnull pointer stored here comes from Box::into_raw()
    let prev = unsafe { Box::from_raw(prev) };
    // Safety: The value restored here was returned from a previous sigaction call
    unsafe { signal::sigaction(signal::SIGBUS, &prev.sigbus)? };
    unsafe { signal::sigaction(signal::SIGSEGV, &prev.sigsegv)? };
    // We want to avoid freeing memory inside the handler, so just leak it
    // This is fine since we're crashing anyway at this point
    if inside_signal_handler {
        Box::leak(prev);
    }
    Ok(())
}

/// Allocates a signal altstack, and puts a guard page at the end.
/// Inspired by https://github.com/rust-lang/rust/pull/69969/files
unsafe fn create_alt_stack() -> anyhow::Result<()> {
    if ALTSTACK_INIT.load(SeqCst) {
        return Ok(());
    }

    // Ensure that the altstack size is the greater of 16 pages or SIGSTKSZ. This is necessary
    // because the default SIGSTKSZ is 8KB, which we're starting to run into. This new size is
    // arbitrary, but at least it's large enough for our purposes, and yet a small enough part of
    // the process RSS that it shouldn't be a problem.
    let page_size = page_size::get();
    let sigalstack_base_size = std::cmp::max(SIGSTKSZ, 16 * page_size);
    let stackp = mmap(
        ptr::null_mut(),
        sigalstack_base_size + page_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANON,
        -1,
        0,
    );
    anyhow::ensure!(
        stackp != MAP_FAILED,
        "failed to allocate an alternative stack"
    );
    let guard_result = libc::mprotect(stackp, page_size, PROT_NONE);
    anyhow::ensure!(
        guard_result == 0,
        "failed to set up alternative stack guard page"
    );
    let stackp = stackp.add(page_size);

    let stack = libc::stack_t {
        ss_sp: stackp,
        ss_flags: 0,
        ss_size: sigalstack_base_size,
    };
    let rval = sigaltstack(&stack, ptr::null_mut());
    anyhow::ensure!(rval == 0, "sigaltstack failed {rval}");
    ALTSTACK_INIT.store(true, SeqCst);
    Ok(())
}
