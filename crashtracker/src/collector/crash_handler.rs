// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
#![allow(deprecated)]

use super::emitters::emit_crashreport;
use crate::crash_info::CrashtrackerMetadata;
use crate::shared::configuration::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use anyhow::Context;
use libc::{
    _exit, c_void, dup2, execve, mmap, sigaltstack, siginfo_t, vfork, MAP_ANON, MAP_FAILED,
    MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE, SIGSTKSZ,
};
use nix::poll::{poll, PollFd, PollFlags};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler};
use nix::sys::socket;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, Pid};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::{
    io::{BorrowedFd, FromRawFd, IntoRawFd, RawFd},
    net::UnixStream,
};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct OldHandlers {
    pub sigbus: SigAction,
    pub sigsegv: SigAction,
}

struct Receiver {
    receiver_uds: RawFd,
    receiver_pid: i32,
}

// Provides a lexically-scoped guard for signals
// During execution of the signal handler, it cannot be guaranteed that the signal is handled
// without SA_NODEFER, thus it also cannot be guaranteed that signals like SIGCHLD and SIGPIPE will
// _not_ be emitted during this handler as a result of the handler itself. At the same time, it
// isn't known whether it is safe to merely block all signals, as the user's own handler will be
// given the chance to execute after ours. Thus, we need to prevent the emission of signals we
// might create (and cannot be created during a signal handler except by our own execution) and
// defer any other signals.
// TODO this forces dynamic allocation in the signal handler
struct SaGuard<const N: usize> {
    old_sigactions: [(signal::Signal, signal::SigAction); N],
    old_sigmask: signal::SigSet,
}

impl<const N: usize> SaGuard<N> {
    fn new(signals: &[signal::Signal; N]) -> anyhow::Result<Self> {
        // Create an empty signal set for suppressing signals
        let mut suppressed_signals = signal::SigSet::empty();
        for signal in signals {
            suppressed_signals.add(*signal);
        }

        // Save the current signal mask and block all signals except the suppressed ones
        let mut old_sigmask = signal::SigSet::empty();
        signal::sigprocmask(
            signal::SigmaskHow::SIG_BLOCK,
            Some(&suppressed_signals),
            Some(&mut old_sigmask),
        )?;

        // Initialize array for saving old signal actions
        let mut old_sigactions = [(
            signal::Signal::SIGINT,
            SigAction::new(
                SigHandler::SigIgn,
                SaFlags::empty(),
                signal::SigSet::empty(),
            ),
        ); N];

        // Set SIG_IGN for the specified signals and save old handlers
        for (i, &signal) in signals.iter().enumerate() {
            let old_sigaction = unsafe {
                signal::sigaction(
                    signal,
                    &SigAction::new(
                        SigHandler::SigIgn,
                        SaFlags::empty(),
                        signal::SigSet::empty(),
                    ),
                )?
            };
            old_sigactions[i] = (signal, old_sigaction);
        }

        Ok(Self {
            old_sigactions,
            old_sigmask,
        })
    }
}

impl<const N: usize> Drop for SaGuard<N> {
    fn drop(&mut self) {
        // Restore the original signal actions
        for &(signal, old_sigaction) in &self.old_sigactions {
            unsafe {
                let _ = signal::sigaction(signal, &old_sigaction);
            }
        }

        // Restore the original signal mask
        let _ = signal::sigprocmask(
            signal::SigmaskHow::SIG_SETMASK,
            Some(&self.old_sigmask),
            None,
        );
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
fn reap_child_non_blocking(pid: Pid, timeout_ms: u64) -> anyhow::Result<bool> {
    let timeout = Duration::from_millis(timeout_ms);
    let start_time = Instant::now();

    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if Instant::now().duration_since(start_time) > timeout {
                    return Err(anyhow::anyhow!("Timeout waiting for child process to exit"));
                }
            }
            Ok(_status) => {
                return Ok(true);
            }
            Err(nix::Error::ECHILD) => {
                // Non-availability of the specified process is weird, since we should have
                // exclusive access to reaping its exit, but at the very least means there is
                // nothing further for us to do.
                return Ok(true);
            }
            _ => {
                return Err(anyhow::anyhow!("Error waiting for child process to exit"));
            }
        }
    }
}

/// Wrapper around the child process that will run the crash receiver
/// TODO the execve arg coersion requires allocation, which we should avoid from a signal handler.
fn run_receiver_child(
    uds_parent: RawFd,
    uds_child: RawFd,
    stderr: RawFd,
    stdout: RawFd,
    config: &CrashtrackerReceiverConfig,
) -> ! {
    // File descriptor management
    unsafe {
        dup2(uds_child, 0);
        dup2(stdout, 1);
        dup2(stderr, 2);
    }

    // Close unused file descriptors
    let _ = close(uds_parent);
    let _ = close(uds_child);
    let _ = close(stderr);
    let _ = close(stdout);

    // Conform inputs to execve calling convention
    // Bind the CString to a variable to extend its lifetime
    let binary_path = CString::new(config.path_to_receiver_binary.as_str())
        .expect("Failed to convert binary path to CString");

    // Collect arguments as CString and store them in a Vec to extend their lifetimes
    let args_cstrings: Vec<CString> = config
        .args
        .iter()
        .map(|s| CString::new(s.as_str()).expect("Failed to convert argument to CString"))
        .collect();

    // Collect pointers to each argument
    let mut args_ptrs: Vec<*const i8> = args_cstrings.iter().map(|arg| arg.as_ptr()).collect();
    args_ptrs.push(std::ptr::null()); // Null-terminate the argument list

    // Collect environment variables as CString and store them in a Vec to extend their lifetimes
    let mut env_vars_cstrings = Vec::with_capacity(config.env.len());
    for (key, value) in &config.env {
        let env_str = format!("{key}={value}");
        let cstring =
            CString::new(env_str).expect("Failed to convert environment variable to CString");
        env_vars_cstrings.push(cstring);
    }
    let mut env_vars_ptrs: Vec<*const i8> =
        env_vars_cstrings.iter().map(|env| env.as_ptr()).collect();
    env_vars_ptrs.push(std::ptr::null()); // Null-terminate the environment variable list

    // Change into the crashtracking receiver
    unsafe {
        execve(
            binary_path.as_ptr(),   // Binary path CString pointer
            args_ptrs.as_ptr(),     // Argument list pointers
            env_vars_ptrs.as_ptr(), // Environment variable pointers
        );
    }

    // If we reach this point, execve failed, so just exit
    unsafe {
        _exit(-1);
    }
}

fn run_receiver_parent(
    _uds_parent: RawFd,
    uds_child: RawFd,
    _stderr: RawFd,
    _stdout: RawFd,
    _config: &CrashtrackerReceiverConfig,
) {
    let _ = close(uds_child);
}

fn wait_for_pollhup(target_fd: RawFd, timeout_ms: i32) -> anyhow::Result<bool> {
    // Need to convert the RawFd into a BorrowedFd to satisfy the PollFd prototype
    let target_fd = unsafe { BorrowedFd::borrow_raw(target_fd) };
    let poll_fd = PollFd::new(&target_fd, PollFlags::POLLHUP);

    match poll(&mut [poll_fd], timeout_ms)? {
        -1 => Err(anyhow::anyhow!("poll failed")),
        0 => Ok(false),
        _ => match poll_fd
            .revents()
            .ok_or_else(|| anyhow::anyhow!("No revents found"))?
        {
            revents if revents.contains(PollFlags::POLLHUP) => Ok(true),
            _ => Err(anyhow::anyhow!("poll returned unexpected result")),
        },
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
    match unsafe { vfork() } {
        0 => {
            // Child (noreturn)
            run_receiver_child(uds_parent, uds_child, stderr, stdout, config);
        }
        pid if pid > 0 => {
            // Parent
            run_receiver_parent(uds_parent, uds_child, stderr, stdout, config);
            Ok(Receiver {
                receiver_uds: uds_parent,
                receiver_pid: pid,
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
    let box_ptr = Box::into_raw(Box::new(config));
    let old = RECEIVER_CONFIG.swap(box_ptr, SeqCst);
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
    let _ = handle_posix_signal_impl(signum);

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

fn handle_posix_signal_impl(signum: i32) -> anyhow::Result<()> {
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

    // Leak config, receiver config, and metadata to avoid calling 'drop' during a crash
    let config = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { config.as_ref().context("No crashtracking receiver")? };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { metadata_ptr.as_ref().context("metadata ptr")? };

    let receiver_config = RECEIVER_CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(
        !receiver_config.is_null(),
        "No crashtracking receiver config"
    );
    let receiver_config = unsafe { receiver_config.as_ref().context("receiver config")? };

    // During the execution of this signal handler, block ALL other signals, especially because we
    // cannot control whether or not we run with SA_NODEFER (crashtracker might have been chained).
    // The especially problematic signals are SIGCHLD and SIGPIPE, which are possibly delivered due
    // to the execution of this handler.
    // SaGuard ensures that signals are restored to their original state even if control flow is
    // disrupted.
    let res: Result<(), anyhow::Error>;
    {
        // `_guard` is a lexically-scoped object whose instantiation blocks/suppresses signals and
        // whose destruction restores the original state
        let _guard = SaGuard::<2>::new(&[signal::SIGCHLD, signal::SIGPIPE])?;

        // Launch the receiver process
        let receiver = make_receiver(receiver_config)?;

        // Creating this tream means the underlying RawFD is now owned by the stream, so
        // we shouldn't close it manually.
        let mut unix_stream = unsafe { UnixStream::from_raw_fd(receiver.receiver_uds) };

        res = emit_crashreport(
            &mut unix_stream,
            config,
            config_str,
            metadata_string,
            signum,
        );

        let _ = unix_stream.flush();
        unix_stream
            .shutdown(std::net::Shutdown::Write)
            .context("Could not shutdown writing on the stream")?;

        // We have to wait for the receiver process and reap its exit status.
        let _ = wait_for_pollhup(receiver.receiver_uds, 1000);

        // Either the receiver is done, it timed out, or something failed.
        // In any case, can't guarantee that the receiver will exit.
        // SIGKILL will ensure that the process ends eventually, but there's
        // no bound on that time.
        // We emit SIGKILL and try to reap its exit status for 25ms, then just give up.
        unsafe {
            libc::kill(receiver.receiver_pid, libc::SIGKILL);
        }
        let receiver_pid_as_pid = Pid::from_raw(receiver.receiver_pid);
        let _ = reap_child_non_blocking(receiver_pid_as_pid, 25);
    } // Drop the guard

    res
}

/// Registers UNIX signal handlers to detect program crashes.
/// This function can be called multiple times and will be idempotent: it will
/// only create and set the handlers once.
/// However, note the restriction below:
/// PRECONDITIONS:
///     The signal handlers should be restored before removing the receiver.
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
pub fn register_crash_handlers(create_alt_stack: bool) -> anyhow::Result<()> {
    if !OLD_HANDLERS.load(SeqCst).is_null() {
        return Ok(());
    }

    unsafe {
        if create_alt_stack {
            set_alt_stack()?;
        }
        let sigbus = register_signal_handler(signal::SIGBUS)?;
        let sigsegv = register_signal_handler(signal::SIGSEGV)?;
        let boxed_ptr = Box::into_raw(Box::new(OldHandlers { sigbus, sigsegv }));

        let res = OLD_HANDLERS.compare_exchange(ptr::null_mut(), boxed_ptr, SeqCst, SeqCst);
        anyhow::ensure!(
            res.is_ok(),
            "TOCTTOU error in crashtracker::register_crash_handlers"
        );
    }
    Ok(())
}

unsafe fn register_signal_handler(signal_type: signal::Signal) -> anyhow::Result<SigAction> {
    // https://www.gnu.org/software/libc/manual/html_node/Flags-for-Sigaction.html
    // ===============
    // If this flag is set for a particular signal number, the system uses the
    // signal stack when delivering that kind of signal.
    // See Using a Separate Signal Stack.
    // If a signal with this flag arrives and you have not set a signal stack,
    // the normal user stack is used instead, as if the flag had not been set.
    // ===============
    // This implies that it is always safe to set SA_ONSTACK.
    let sig_action = SigAction::new(
        SigHandler::SigAction(handle_posix_sigaction),
        SaFlags::SA_NODEFER | SaFlags::SA_ONSTACK,
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
unsafe fn set_alt_stack() -> anyhow::Result<()> {
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
